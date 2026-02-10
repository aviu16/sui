// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::{TestCaseImpl, TestContext};
use anyhow::Context;
use async_trait::async_trait;
use jsonrpsee::{core::client::ClientT, http_client::HttpClientBuilder, rpc_params};
use sui_json_rpc_types::SuiTransactionBlockResponseOptions;
use sui_test_transaction_builder::make_staking_transaction;
use sui_types::gas_coin::GAS;
use sui_types::transaction_driver_types::ExecuteTransactionRequestType;
use tracing::info;

pub struct GovernanceStakingTest;

#[async_trait]
impl TestCaseImpl for GovernanceStakingTest {
    fn name(&self) -> &'static str {
        "GovernanceStaking"
    }

    fn description(&self) -> &'static str {
        "Test governance, staking, and supply APIs via gRPC and JSON-RPC"
    }

    fn rpcs_tested(&self) -> Vec<&'static str> {
        vec![
            "suix_getLatestSuiSystemState",
            "suix_getCommitteeInfo",
            "suix_getReferenceGasPrice",
            "suix_getValidatorsApy",
            "suix_getStakes",
            "suix_getTotalSupply",
        ]
    }

    async fn run(&self, ctx: &mut TestContext) -> Result<(), anyhow::Error> {
        let client = ctx.clone_fullnode_client();
        let grpc = ctx.get_grpc_client();

        // === System State ===
        info!("Testing system state queries");
        let json_state = client
            .governance_api()
            .get_latest_sui_system_state()
            .await
            .context("get_latest_sui_system_state")?;
        assert!(
            !json_state.active_validators.is_empty(),
            "Should have active validators"
        );

        let grpc_state = grpc
            .get_system_state_summary(None)
            .await
            .context("gRPC get_system_state_summary")?;
        assert!(
            !grpc_state.active_validators.is_empty(),
            "gRPC should have active validators"
        );

        assert_eq!(
            json_state.epoch, grpc_state.epoch,
            "Epoch should match between JSON-RPC and gRPC"
        );
        assert_eq!(
            json_state.active_validators.len(),
            grpc_state.active_validators.len(),
            "Validator count should match"
        );
        assert_eq!(
            json_state.reference_gas_price, grpc_state.reference_gas_price,
            "Reference gas price should match"
        );
        info!(
            "System state cross-verified: epoch={}, validators={}, rgp={}",
            json_state.epoch,
            json_state.active_validators.len(),
            json_state.reference_gas_price
        );

        // === Committee Info ===
        info!("Testing committee info");
        let json_committee = client
            .governance_api()
            .get_committee_info(None)
            .await
            .context("get_committee_info")?;
        assert!(
            !json_committee.validators.is_empty(),
            "Committee should have validators"
        );

        let grpc_committee = grpc
            .get_committee(None)
            .await
            .context("gRPC get_committee")?;

        assert_eq!(
            json_committee.epoch,
            grpc_committee.epoch(),
            "Committee epoch should match"
        );
        assert_eq!(
            json_committee.validators.len(),
            grpc_committee.num_members(),
            "Committee member count should match"
        );
        info!(
            "Committee cross-verified: epoch={}, members={}",
            json_committee.epoch,
            json_committee.validators.len()
        );

        // === Reference Gas Price ===
        info!("Testing reference gas price");
        let json_rgp = client
            .governance_api()
            .get_reference_gas_price()
            .await
            .context("get_reference_gas_price")?;
        let grpc_rgp = grpc
            .get_reference_gas_price()
            .await
            .context("gRPC get_reference_gas_price")?;
        assert!(json_rgp > 0, "Gas price should be > 0");
        assert_eq!(
            json_rgp, grpc_rgp,
            "Reference gas price should match between JSON-RPC and gRPC"
        );
        info!("Reference gas price cross-verified: {}", json_rgp);

        // === Total Supply ===
        info!("Testing total supply");
        let supply = client
            .coin_read_api()
            .get_total_supply("0x2::sui::SUI".to_string())
            .await
            .context("get_total_supply")?;
        assert!(supply.value > 0, "Total supply should be > 0");

        let coin_info = grpc
            .get_coin_info(&GAS::type_())
            .await
            .context("gRPC get_coin_info")?;
        let grpc_total_supply = coin_info
            .treasury
            .as_ref()
            .expect("SUI should have treasury info")
            .total_supply
            .expect("SUI should have total_supply");
        assert_eq!(
            supply.value, grpc_total_supply,
            "Total supply should match between JSON-RPC ({}) and gRPC ({})",
            supply.value, grpc_total_supply
        );
        info!("Total supply cross-verified: {}", supply.value);

        // === Validators APY (JSON-RPC only, no SDK method) ===
        // Deserialize as Value first because devnet may return null for apy fields
        // when there isn't enough epoch history to compute APY.
        info!("Testing validators APY");
        let fn_rpc_url = ctx.get_fullnode_rpc_url();
        let rpc_client = HttpClientBuilder::default()
            .build(fn_rpc_url)
            .context("build jsonrpsee client")?;
        let apy_response: serde_json::Value = rpc_client
            .request("suix_getValidatorsApy", rpc_params![])
            .await
            .context("suix_getValidatorsApy")?;
        let apys_array = apy_response["apys"]
            .as_array()
            .expect("apys should be an array");
        assert!(!apys_array.is_empty(), "Should have validator APYs");
        assert_eq!(
            apys_array.len(),
            json_state.active_validators.len(),
            "APY count should match active validator count"
        );
        for entry in apys_array {
            assert!(
                entry.get("address").is_some(),
                "Each APY entry should have an address"
            );
            if let Some(apy) = entry["apy"].as_f64() {
                assert!(apy >= 0.0, "APY should be non-negative");
            }
        }
        info!("Validators APY verified: {} validator(s)", apys_array.len());

        // === Staking ===
        info!("Testing staking and stake queries");
        ctx.get_sui_from_faucet(Some(1)).await;
        let wallet_addr = ctx.get_wallet_address();

        // Pre-check: no stakes yet for this wallet
        let pre_stakes = client
            .governance_api()
            .get_stakes(wallet_addr)
            .await
            .context("get_stakes (pre-check)")?;
        let pre_grpc_stakes = grpc
            .list_delegated_stake(wallet_addr)
            .await
            .context("gRPC list_delegated_stake (pre-check)")?;
        info!(
            "Pre-staking: JSON-RPC stakes={}, gRPC stakes={}",
            pre_stakes.len(),
            pre_grpc_stakes.len()
        );

        // Execute staking transaction
        let validator_addr = json_state
            .active_validators
            .first()
            .expect("Should have at least one validator")
            .sui_address;
        let txn = make_staking_transaction(ctx.get_wallet(), validator_addr).await;
        let digest = *txn.digest();

        client
            .quorum_driver_api()
            .execute_transaction_block(
                txn,
                SuiTransactionBlockResponseOptions::new().with_effects(),
                Some(ExecuteTransactionRequestType::WaitForLocalExecution),
            )
            .await
            .context("execute staking transaction")?;
        ctx.let_fullnode_sync(vec![digest], 10).await;
        info!("Staking transaction executed: {}", digest);

        // JSON-RPC: verify stake appeared
        let json_stakes = client
            .governance_api()
            .get_stakes(wallet_addr)
            .await
            .context("get_stakes (post-staking)")?;
        assert!(
            !json_stakes.is_empty(),
            "Should have at least one stake after staking"
        );
        let matching_stake = json_stakes
            .iter()
            .find(|s| s.validator_address == validator_addr);
        assert!(
            matching_stake.is_some(),
            "Should find stake for validator {}",
            validator_addr
        );
        info!(
            "JSON-RPC getStakes verified: {} delegation(s)",
            json_stakes.len()
        );

        // gRPC: verify stake appeared
        let grpc_stakes = grpc
            .list_delegated_stake(wallet_addr)
            .await
            .context("gRPC list_delegated_stake (post-staking)")?;
        assert!(
            !grpc_stakes.is_empty(),
            "gRPC should report delegated stakes"
        );

        // Cross-verify stake counts
        let json_stake_count: usize = json_stakes.iter().map(|s| s.stakes.len()).sum();
        assert_eq!(
            json_stake_count,
            grpc_stakes.len(),
            "Individual stake count should match: JSON-RPC={}, gRPC={}",
            json_stake_count,
            grpc_stakes.len()
        );
        info!(
            "Staking cross-verified: {} stake(s) via both JSON-RPC and gRPC",
            grpc_stakes.len()
        );

        Ok(())
    }
}
