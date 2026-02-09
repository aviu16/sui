// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::{TestCaseImpl, TestContext};
use anyhow::Context;
use async_trait::async_trait;
use sui_json_rpc_types::EventFilter;
use sui_sdk::wallet_context::WalletContext;
use sui_test_transaction_builder::{emit_new_random_u128, publish_basics_package};
use sui_types::effects::TransactionEffectsAPI;
use tracing::info;

pub struct EventQueryTest;

#[async_trait]
impl TestCaseImpl for EventQueryTest {
    fn name(&self) -> &'static str {
        "EventQuery"
    }

    fn description(&self) -> &'static str {
        "Test event emission and querying by digest and type filter"
    }

    fn rpcs_tested(&self) -> Vec<&'static str> {
        vec!["sui_getEvents", "suix_queryEvents"]
    }

    async fn run(&self, ctx: &mut TestContext) -> Result<(), anyhow::Error> {
        info!("Testing event emission and querying");

        let sui_objs = ctx.get_sui_from_faucet(Some(1)).await;
        assert!(!sui_objs.is_empty());

        let wallet_context: &WalletContext = ctx.get_wallet();

        // Publish the basics package (same as RandomBeaconTest)
        let package_ref = publish_basics_package(wallet_context).await;
        info!("Basics package published: {:?}", package_ref.0);

        // Emit an event by calling emit_new_random_u128
        let response = emit_new_random_u128(wallet_context, package_ref.0).await;
        assert!(
            response.effects.status().is_ok(),
            "Event emission transaction should succeed: {:?}",
            *response.effects.status()
        );

        let tx_digest = response.transaction.digest();

        // Verify fullnode observes the txn
        ctx.let_fullnode_sync(vec![tx_digest], 5).await;

        // Test get_events by transaction digest
        info!("Testing get_events by transaction digest");
        let client = ctx.get_fullnode_client();
        let events = client
            .event_api()
            .get_events(tx_digest)
            .await
            .context("get_events by transaction digest")?;
        assert!(
            !events.is_empty(),
            "Should have at least one event from the transaction"
        );
        assert_eq!(
            events[0].type_.name.to_string(),
            "RandomU128Event",
            "Event type should be RandomU128Event"
        );
        info!(
            "get_events verified: {} event(s), type={}",
            events.len(),
            events[0].type_.name
        );

        // Test query_events by event type filter
        info!("Testing query_events by MoveEventType filter");
        let event_type = events[0].type_.clone();
        let event_page = client
            .event_api()
            .query_events(EventFilter::MoveEventType(event_type), None, Some(10), true)
            .await
            .context("query_events by MoveEventType filter")?;
        assert!(
            !event_page.data.is_empty(),
            "Should find at least one event matching the type filter"
        );
        assert!(
            event_page.data.iter().any(|e| e.id.tx_digest == tx_digest),
            "Filtered results should include the event from our transaction"
        );
        info!(
            "query_events verified: {} result(s) matching type filter",
            event_page.data.len()
        );

        Ok(())
    }
}
