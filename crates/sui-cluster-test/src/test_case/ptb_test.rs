// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::{TestCaseImpl, TestContext};
use anyhow::Context;
use async_trait::async_trait;
use sui_sdk::wallet_context::WalletContext;
use sui_test_transaction_builder::TestTransactionBuilder;
use sui_types::base_types::SuiAddress;
use sui_types::crypto::{AccountKeyPair, get_key_pair};
use sui_types::effects::TransactionEffectsAPI;
use tracing::info;

pub struct PtbTest;

#[async_trait]
impl TestCaseImpl for PtbTest {
    fn name(&self) -> &'static str {
        "ProgrammableTransactionBlock"
    }

    fn description(&self) -> &'static str {
        "Test executing a programmable transaction block with multiple operations"
    }

    async fn run(&self, ctx: &mut TestContext) -> Result<(), anyhow::Error> {
        info!("Testing programmable transaction block execution");

        let sui_objs = ctx.get_sui_from_faucet(Some(1)).await;
        assert!(!sui_objs.is_empty(), "Should have at least one gas coin");

        let wallet: &WalletContext = ctx.get_wallet();
        let sender = ctx.get_wallet_address();
        let rgp = ctx.get_grpc_client().get_reference_gas_price().await?;
        let (recipient, _): (_, AccountKeyPair) = get_key_pair();

        // Get a gas object for the PTB
        let gas_obj = wallet
            .get_one_gas_object_owned_by_address(sender)
            .await
            .context("fetching gas object for first PTB")?
            .expect("Should have a gas object");

        // Build a PTB that splits a coin and transfers part to a recipient
        let tx_data = TestTransactionBuilder::new(sender, gas_obj, rgp)
            .transfer_sui(Some(1000), recipient)
            .build();

        let response = ctx.grpc_sign_and_execute(tx_data, "PTB transfer").await;

        let effects = &response.effects;
        assert!(
            !effects.mutated().is_empty() || !effects.created().is_empty(),
            "PTB should have mutated or created objects"
        );

        // Verify fullnode observes the txn
        let tx_digest = *effects.transaction_digest();
        ctx.let_fullnode_sync(vec![tx_digest], 5).await;

        // Verify the recipient received funds
        let recipient_addr: SuiAddress = recipient;
        let recipient_change = response
            .balance_changes
            .iter()
            .find(|b| SuiAddress::from(b.address) == recipient_addr);
        assert!(
            recipient_change.is_some(),
            "Recipient should have a balance change"
        );
        assert!(
            recipient_change.unwrap().amount > 0,
            "Recipient should receive positive balance"
        );
        info!(
            "PTB transfer verified: {} object(s) created/mutated, recipient received funds",
            effects.created().len() + effects.mutated().len()
        );

        // Test a second PTB: transfer to a different recipient
        info!("Testing PTB with transfer to second recipient");
        let (recipient2, _): (_, AccountKeyPair) = get_key_pair();
        let gas_obj = wallet
            .get_one_gas_object_owned_by_address(sender)
            .await
            .context("fetching gas object for second PTB")?
            .expect("Should have a gas object for second PTB");

        let tx_data = TestTransactionBuilder::new(sender, gas_obj, rgp)
            .transfer_sui(Some(500), recipient2)
            .build();

        let response = ctx
            .grpc_sign_and_execute(tx_data, "PTB transfer to second recipient")
            .await;

        let effects = &response.effects;
        assert!(
            !effects.created().is_empty(),
            "Transfer should create a new coin for the recipient"
        );

        let tx_digest = *effects.transaction_digest();
        ctx.let_fullnode_sync(vec![tx_digest], 5).await;
        info!(
            "Second PTB verified: {} object(s) created for recipient2",
            effects.created().len()
        );

        Ok(())
    }
}
