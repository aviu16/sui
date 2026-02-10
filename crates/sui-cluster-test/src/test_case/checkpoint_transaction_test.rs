// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::{TestCaseImpl, TestContext};
use anyhow::Context;
use async_trait::async_trait;
use sui_json_rpc_types::{
    CheckpointId, SuiTransactionBlockDataAPI, SuiTransactionBlockResponseOptions,
};
use sui_types::effects::TransactionEffectsAPI;
use sui_types::transaction::TransactionDataAPI;
use sui_types::transaction_driver_types::ExecuteTransactionRequestType;
use tracing::info;

pub struct CheckpointTransactionTest;

#[async_trait]
impl TestCaseImpl for CheckpointTransactionTest {
    fn name(&self) -> &'static str {
        "CheckpointTransaction"
    }

    fn description(&self) -> &'static str {
        "Test checkpoint and transaction read APIs via gRPC and JSON-RPC"
    }

    fn rpcs_tested(&self) -> Vec<&'static str> {
        vec![
            "sui_getTransactionBlock",
            "sui_multiGetTransactionBlocks",
            "sui_getCheckpoint",
            "sui_getCheckpoints",
            "sui_getLatestCheckpointSequenceNumber",
            "sui_getTotalTransactionBlocks",
        ]
    }

    async fn run(&self, ctx: &mut TestContext) -> Result<(), anyhow::Error> {
        // Setup: create and execute two transactions
        ctx.get_sui_from_faucet(Some(1)).await;
        let mut txns = ctx.make_transactions(2).await;
        assert!(
            txns.len() >= 2,
            "Need at least 2 transactions, got {}",
            txns.len()
        );

        let txn1 = txns.swap_remove(0);
        let txn2 = txns.swap_remove(0);
        let digest1 = *txn1.digest();
        let digest2 = *txn2.digest();

        let client = ctx.clone_fullnode_client();
        let opts = SuiTransactionBlockResponseOptions::new().with_effects();

        client
            .quorum_driver_api()
            .execute_transaction_block(
                txn1,
                opts.clone(),
                Some(ExecuteTransactionRequestType::WaitForLocalExecution),
            )
            .await
            .context("execute txn1")?;
        client
            .quorum_driver_api()
            .execute_transaction_block(
                txn2,
                opts,
                Some(ExecuteTransactionRequestType::WaitForLocalExecution),
            )
            .await
            .context("execute txn2")?;
        ctx.let_fullnode_sync(vec![digest1, digest2], 10).await;

        // === Checkpoint Reads ===
        info!("Testing checkpoint read APIs");

        let json_latest_seq = client
            .read_api()
            .get_latest_checkpoint_sequence_number()
            .await
            .context("get_latest_checkpoint_sequence_number")?;
        assert!(json_latest_seq > 0);
        info!(
            "JSON-RPC getLatestCheckpointSequenceNumber: {}",
            json_latest_seq
        );

        let mut grpc = ctx.get_grpc_client();
        let grpc_latest = grpc
            .get_latest_checkpoint()
            .await
            .context("gRPC get_latest_checkpoint")?;
        assert!(grpc_latest.sequence_number > 0);
        info!(
            "gRPC get_latest_checkpoint: seq={}",
            grpc_latest.sequence_number
        );

        // Cross-verify: gRPC may be slightly ahead
        assert!(
            grpc_latest.sequence_number >= json_latest_seq,
            "gRPC latest ({}) should be >= JSON-RPC latest ({})",
            grpc_latest.sequence_number,
            json_latest_seq
        );
        info!("Checkpoint sequence cross-verified");

        // Fetch specific checkpoint by sequence number
        let checkpoint_seq = json_latest_seq;
        let json_checkpoint = client
            .read_api()
            .get_checkpoint(CheckpointId::SequenceNumber(checkpoint_seq))
            .await
            .context("get_checkpoint")?;
        assert_eq!(json_checkpoint.sequence_number, checkpoint_seq);
        info!(
            "JSON-RPC getCheckpoint: seq={}, epoch={}, digest={}",
            json_checkpoint.sequence_number, json_checkpoint.epoch, json_checkpoint.digest
        );

        // gRPC: get_checkpoint_summary for same sequence
        let grpc_checkpoint = grpc
            .get_checkpoint_summary(checkpoint_seq)
            .await
            .context("gRPC get_checkpoint_summary")?;

        // Cross-verify checkpoint fields
        assert_eq!(
            json_checkpoint.sequence_number, grpc_checkpoint.sequence_number,
            "Checkpoint sequence should match"
        );
        assert_eq!(
            json_checkpoint.epoch, grpc_checkpoint.epoch,
            "Checkpoint epoch should match"
        );
        assert_eq!(
            json_checkpoint.digest,
            *grpc_checkpoint.digest(),
            "Checkpoint digest should match"
        );
        info!("Checkpoint cross-verified between JSON-RPC and gRPC");

        // Paginated checkpoints
        let checkpoint_page = client
            .read_api()
            .get_checkpoints(None, Some(3), false)
            .await
            .context("get_checkpoints")?;
        assert!(!checkpoint_page.data.is_empty());
        assert!(checkpoint_page.data.len() <= 3);
        for window in checkpoint_page.data.windows(2) {
            assert!(
                window[0].sequence_number < window[1].sequence_number,
                "Checkpoints should be in ascending order"
            );
        }
        info!(
            "getCheckpoints verified: {} checkpoint(s) in ascending order",
            checkpoint_page.data.len()
        );

        // gRPC: get_full_checkpoint
        let full_checkpoint = grpc
            .get_full_checkpoint(checkpoint_seq)
            .await
            .context("gRPC get_full_checkpoint")?;
        assert_eq!(
            full_checkpoint.summary.sequence_number, checkpoint_seq,
            "Full checkpoint sequence should match"
        );
        info!("gRPC get_full_checkpoint verified: seq={}", checkpoint_seq);

        // === Transaction Reads ===
        info!("Testing transaction read APIs");

        let total_txns = client
            .read_api()
            .get_total_transaction_blocks()
            .await
            .context("get_total_transaction_blocks")?;
        assert!(total_txns > 0);
        info!("getTotalTransactionBlocks: {}", total_txns);

        // JSON-RPC: getTransactionBlock with all options
        let json_tx = client
            .read_api()
            .get_transaction_with_options(
                digest1,
                SuiTransactionBlockResponseOptions::new()
                    .with_effects()
                    .with_input()
                    .with_events(),
            )
            .await
            .context("get_transaction_with_options")?;
        assert_eq!(json_tx.digest, digest1);
        assert!(json_tx.effects.is_some(), "Should include effects");
        assert!(
            json_tx.transaction.is_some(),
            "Should include transaction input"
        );
        info!("getTransactionBlock verified: digest={}", digest1);

        // gRPC: get_transaction
        let grpc_tx = grpc
            .get_transaction(&digest1)
            .await
            .context("gRPC get_transaction")?;
        assert_eq!(*grpc_tx.effects.transaction_digest(), digest1);

        // Cross-verify sender
        let json_sender = json_tx.transaction.as_ref().unwrap().data.sender();
        let grpc_sender = grpc_tx.transaction.sender();
        assert_eq!(
            *json_sender, grpc_sender,
            "Transaction sender should match between JSON-RPC and gRPC"
        );
        info!("Transaction cross-verified: sender={}", grpc_sender);

        // JSON-RPC: multiGetTransactionBlocks
        let multi_results = client
            .read_api()
            .multi_get_transactions_with_options(
                vec![digest1, digest2],
                SuiTransactionBlockResponseOptions::new().with_effects(),
            )
            .await
            .context("multi_get_transactions_with_options")?;
        assert_eq!(multi_results.len(), 2);
        let returned_digests: Vec<_> = multi_results.iter().map(|r| r.digest).collect();
        assert!(
            returned_digests.contains(&digest1),
            "Multi-get should include digest1"
        );
        assert!(
            returned_digests.contains(&digest2),
            "Multi-get should include digest2"
        );
        info!(
            "multiGetTransactionBlocks verified: {} result(s)",
            multi_results.len()
        );

        // === Bonus: gRPC batch object reads ===
        info!("Testing gRPC batch object reads");

        // Get object IDs from the transaction effects
        let created = grpc_tx.effects.created();
        if created.len() >= 2 {
            let obj_ids: Vec<_> = created.iter().map(|o| o.0.0).collect();
            let batch_result = grpc
                .batch_get_objects(&obj_ids[..2])
                .await
                .context("gRPC batch_get_objects")?;
            assert_eq!(batch_result.len(), 2, "Should return 2 objects");
            info!(
                "gRPC batch_get_objects verified: {} object(s)",
                batch_result.len()
            );
        }

        Ok(())
    }
}
