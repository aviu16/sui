// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::{TestCaseImpl, TestContext};
use anyhow::Context;
use async_trait::async_trait;
use sui_json_rpc_types::{
    SuiExecutionStatus, SuiTransactionBlockEffectsAPI, SuiTransactionBlockResponseOptions,
    SuiTransactionBlockResponseQuery, TransactionFilter,
};
use sui_types::{
    base_types::TransactionDigest, transaction_driver_types::ExecuteTransactionRequestType,
};
use tracing::info;

pub struct FullNodeExecuteTransactionTest;

impl FullNodeExecuteTransactionTest {
    async fn verify_transaction(ctx: &TestContext, tx_digest: TransactionDigest) {
        let mut grpc = ctx.get_grpc_client();
        grpc.get_transaction(&tx_digest).await.unwrap_or_else(|e| {
            panic!(
                "Failed get transaction {:?} from fullnode: {:?}",
                tx_digest, e
            )
        });
    }
}

#[async_trait]
impl TestCaseImpl for FullNodeExecuteTransactionTest {
    fn name(&self) -> &'static str {
        "FullNodeExecuteTransaction"
    }

    fn description(&self) -> &'static str {
        "Test executing transaction via Fullnode Quorum Driver"
    }

    fn rpcs_tested(&self) -> Vec<&'static str> {
        vec!["sui_executeTransactionBlock", "suix_queryTransactionBlocks"]
    }

    async fn run(&self, ctx: &mut TestContext) -> Result<(), anyhow::Error> {
        // Test checkpoint and protocol queries via gRPC
        info!("Testing checkpoint and protocol queries via gRPC");
        let mut grpc = ctx.get_grpc_client();

        let checkpoint = grpc
            .get_latest_checkpoint()
            .await
            .context("get_latest_checkpoint")?;
        assert!(
            checkpoint.sequence_number > 0,
            "Latest checkpoint sequence number should be > 0"
        );
        info!(
            "Checkpoint verified: seq={}, digest={}",
            checkpoint.sequence_number,
            checkpoint.digest()
        );

        let protocol_config = grpc
            .get_protocol_config(None)
            .await
            .context("get_protocol_config")?;
        let protocol_version = protocol_config
            .protocol_version
            .expect("Protocol version should be present");
        assert!(protocol_version > 0, "Protocol version should be > 0");
        info!("Protocol config verified: version={}", protocol_version);

        let chain_id = grpc
            .get_chain_identifier()
            .await
            .context("get_chain_identifier")?;
        let chain_id_str = chain_id.to_string();
        assert!(
            !chain_id_str.is_empty(),
            "Chain identifier should not be empty"
        );
        info!("Chain identifier verified: {}", chain_id_str);

        let txn_count = 4;
        ctx.get_sui_from_faucet(Some(1)).await;

        let mut txns = ctx.make_transactions(txn_count).await;
        assert!(
            txns.len() >= txn_count,
            "Expect at least {} txns, but only got {}. Do we generate enough gas objects during genesis?",
            txn_count,
            txns.len(),
        );

        let fullnode = ctx.get_fullnode_client();

        info!("Test execution with WaitForEffectsCert");
        let txn = txns.swap_remove(0);
        let txn_digest = *txn.digest();

        let response = fullnode
            .quorum_driver_api()
            .execute_transaction_block(
                txn,
                SuiTransactionBlockResponseOptions::new().with_effects(),
                Some(ExecuteTransactionRequestType::WaitForEffectsCert),
            )
            .await?;

        assert!(!response.confirmed_local_execution.unwrap());
        assert_eq!(txn_digest, response.digest);
        let effects = response.effects.unwrap();
        if !matches!(effects.status(), SuiExecutionStatus::Success) {
            panic!(
                "Failed to execute transfer transaction {:?}: {:?}",
                txn_digest,
                effects.status()
            )
        }
        // Verify fullnode observes the txn
        ctx.let_fullnode_sync(vec![txn_digest], 5).await;
        Self::verify_transaction(ctx, txn_digest).await;
        info!("WaitForEffectsCert verified: tx={}", txn_digest);

        info!("Test execution with WaitForLocalExecution");
        let txn = txns.swap_remove(0);
        let txn_digest = *txn.digest();

        let response = fullnode
            .quorum_driver_api()
            .execute_transaction_block(
                txn,
                SuiTransactionBlockResponseOptions::new().with_effects(),
                Some(ExecuteTransactionRequestType::WaitForLocalExecution),
            )
            .await?;
        assert!(response.confirmed_local_execution.unwrap());
        assert_eq!(txn_digest, response.digest);
        let effects = response.effects.unwrap();
        if !matches!(effects.status(), SuiExecutionStatus::Success) {
            panic!(
                "Failed to execute transfer transaction {:?}: {:?}",
                txn_digest,
                effects.status()
            )
        }
        // Unlike in other execution modes, there's no need to wait for the node to sync
        Self::verify_transaction(ctx, txn_digest).await;
        info!("WaitForLocalExecution verified: tx={}", txn_digest);

        // Test suix_queryTransactionBlocks (JSON-RPC only, no gRPC equivalent)
        info!("Testing queryTransactionBlocks");
        let sender = ctx.get_wallet_address();
        let query = SuiTransactionBlockResponseQuery::new_with_filter(
            TransactionFilter::FromAddress(sender),
        );
        let tx_page = fullnode
            .read_api()
            .query_transaction_blocks(query, None, Some(10), true)
            .await
            .context("query_transaction_blocks")?;
        assert!(
            !tx_page.data.is_empty(),
            "Should find at least one transaction from the sender"
        );
        assert!(
            tx_page.data.iter().any(|tx| tx.digest == txn_digest),
            "Query results should include the just-executed transaction"
        );
        info!(
            "queryTransactionBlocks verified: {} result(s)",
            tx_page.data.len()
        );

        // Test response option completeness (JSON-RPC specific)
        info!("Testing transaction response with all options enabled");
        let full_response = fullnode
            .read_api()
            .get_transaction_with_options(
                txn_digest,
                SuiTransactionBlockResponseOptions::new()
                    .with_effects()
                    .with_events()
                    .with_object_changes()
                    .with_balance_changes()
                    .with_input()
                    .with_raw_input(),
            )
            .await
            .context("get_transaction_with_options with all options")?;
        assert!(
            full_response.effects.is_some(),
            "Response should include effects"
        );
        assert!(
            full_response.object_changes.is_some(),
            "Response should include object_changes"
        );
        assert!(
            full_response.balance_changes.is_some(),
            "Response should include balance_changes"
        );
        assert!(
            full_response.transaction.is_some(),
            "Response should include transaction from with_input()"
        );
        assert!(
            !full_response.raw_transaction.is_empty(),
            "Response should include raw_transaction from with_raw_input()"
        );
        info!(
            "Response options verified: effects, object_changes, balance_changes, transaction, raw_transaction all present"
        );

        Ok(())
    }
}
