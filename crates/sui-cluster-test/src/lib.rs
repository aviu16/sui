// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0
use crate::faucet::{FaucetClient, FaucetClientFactory};
use async_trait::async_trait;
use cluster::{Cluster, ClusterFactory};
use config::ClusterTestOpt;
use futures::{StreamExt, stream::FuturesUnordered};
use helper::ObjectChecker;
use jsonrpsee::core::params::ArrayParams;
use jsonrpsee::{core::client::ClientT, http_client::HttpClientBuilder};
use std::sync::Arc;
use std::time::Instant;
use sui_faucet::{CoinInfo, RequestStatus};
use sui_json_rpc_types::{
    SuiExecutionStatus, SuiTransactionBlockEffectsAPI, SuiTransactionBlockResponse,
    SuiTransactionBlockResponseOptions, TransactionBlockBytes,
};
use sui_sdk::wallet_context::WalletContext;
use sui_test_transaction_builder::batch_make_transfer_transactions;
use sui_types::base_types::TransactionDigest;
use sui_types::object::Owner;
use sui_types::sui_system_state::sui_system_state_summary::SuiSystemStateSummary;
use sui_types::transaction_driver_types::ExecuteTransactionRequestType;

use sui_sdk::SuiClient;
use sui_types::gas_coin::GasCoin;
use sui_types::{
    base_types::SuiAddress,
    transaction::{Transaction, TransactionData},
};
use test_case::{
    checkpoint_transaction_test::CheckpointTransactionTest, coin_index_test::CoinIndexTest,
    coin_merge_split_test::CoinMergeSplitTest, dynamic_field_test::DynamicFieldTest,
    event_query_test::EventQueryTest,
    fullnode_build_publish_transaction_test::FullNodeBuildPublishTransactionTest,
    fullnode_execute_transaction_test::FullNodeExecuteTransactionTest,
    governance_staking_test::GovernanceStakingTest, native_transfer_test::NativeTransferTest,
    package_upgrade_test::PackageUpgradeTest, ptb_test::PtbTest,
    random_beacon_test::RandomBeaconTest, shared_object_test::SharedCounterTest,
};
use tokio::time::{self, Duration};
use tracing::{error, info};
use wallet_client::WalletClient;

pub mod cluster;
pub mod config;
pub mod faucet;
pub mod helper;
pub mod test_case;
pub mod wallet_client;

#[allow(unused)]
pub struct TestContext {
    /// Cluster handle that allows access to various components in a cluster
    cluster: Box<dyn Cluster + Sync + Send>,
    /// Client that provides wallet context and fullnode access
    client: WalletClient,
    /// Facuet client that provides faucet access to a test
    faucet: Arc<dyn FaucetClient + Sync + Send>,
}

impl TestContext {
    async fn get_sui_from_faucet(&self, minimum_coins: Option<usize>) -> Vec<GasCoin> {
        let addr = self.get_wallet_address();

        let faucet_response = self.faucet.request_sui_coins(addr).await;
        if let RequestStatus::Failure(e) = faucet_response.status {
            panic!("Failed to get coins from faucet: {e}");
        }

        let coin_info = faucet_response.coins_sent.unwrap_or_default();

        let digests = coin_info
            .iter()
            .map(|coin_info| coin_info.transfer_tx_digest)
            .collect::<Vec<_>>();

        self.let_fullnode_sync(digests, 5).await;

        let gas_coins = self.check_owner_and_into_gas_coin(coin_info, addr).await;

        let minimum_coins = minimum_coins.unwrap_or(1);

        if gas_coins.len() < minimum_coins {
            panic!(
                "Expect to get at least {minimum_coins} Sui Coins for address {addr}, but only got {}",
                gas_coins.len()
            )
        }

        gas_coins
    }

    fn get_context(&self) -> &WalletClient {
        &self.client
    }

    fn get_fullnode_client(&self) -> &SuiClient {
        self.client.get_fullnode_client()
    }

    fn clone_fullnode_client(&self) -> SuiClient {
        self.client.get_fullnode_client().clone()
    }

    fn get_fullnode_rpc_url(&self) -> &str {
        self.cluster.fullnode_url()
    }

    fn get_wallet(&self) -> &WalletContext {
        self.client.get_wallet()
    }

    fn get_grpc_client(&self) -> sui_rpc_api::Client {
        self.client.get_wallet().grpc_client().unwrap()
    }

    async fn grpc_sign_and_execute(
        &self,
        txn_data: TransactionData,
        desc: &str,
    ) -> sui_rpc_api::client::ExecutedTransaction {
        let signature = self.get_context().sign(&txn_data, desc).await;
        let tx = Transaction::from_data(txn_data, vec![signature]);
        self.get_wallet().execute_transaction_must_succeed(tx).await
    }

    async fn get_latest_sui_system_state(&self) -> SuiSystemStateSummary {
        self.client
            .get_fullnode_client()
            .governance_api()
            .get_latest_sui_system_state()
            .await
            .unwrap()
    }

    async fn get_reference_gas_price(&self) -> u64 {
        self.client
            .get_fullnode_client()
            .governance_api()
            .get_reference_gas_price()
            .await
            .unwrap()
    }

    fn get_wallet_address(&self) -> SuiAddress {
        self.client.get_wallet_address()
    }

    /// See `make_transactions_with_wallet_context` for potential caveats
    /// of this helper function.
    pub async fn make_transactions(&self, max_txn_num: usize) -> Vec<Transaction> {
        batch_make_transfer_transactions(self.get_wallet(), max_txn_num).await
    }

    pub async fn build_transaction_remotely(
        &self,
        method: &str,
        params: ArrayParams,
    ) -> anyhow::Result<TransactionData> {
        let fn_rpc_url = self.get_fullnode_rpc_url();
        // TODO cache this?
        let rpc_client = HttpClientBuilder::default().build(fn_rpc_url)?;

        TransactionBlockBytes::to_data(rpc_client.request(method, params).await?)
    }

    async fn sign_and_execute(
        &self,
        txn_data: TransactionData,
        desc: &str,
    ) -> SuiTransactionBlockResponse {
        let signature = self.get_context().sign(&txn_data, desc).await;
        let resp = self
            .get_fullnode_client()
            .quorum_driver_api()
            .execute_transaction_block(
                Transaction::from_data(txn_data, vec![signature]),
                SuiTransactionBlockResponseOptions::new()
                    .with_object_changes()
                    .with_balance_changes()
                    .with_effects()
                    .with_events(),
                Some(ExecuteTransactionRequestType::WaitForLocalExecution),
            )
            .await
            .unwrap_or_else(|e| panic!("Failed to execute transaction for {}. {}", desc, e));
        assert!(
            matches!(
                resp.effects.as_ref().unwrap().status(),
                SuiExecutionStatus::Success
            ),
            "Failed to execute transaction for {desc}: {:?}",
            resp
        );
        resp
    }

    pub async fn setup(options: ClusterTestOpt) -> Result<Self, anyhow::Error> {
        let cluster = ClusterFactory::start(&options).await?;
        let wallet_client = WalletClient::new_from_cluster(&cluster).await;
        let faucet = FaucetClientFactory::new_from_cluster(&cluster).await;
        Ok(Self {
            cluster,
            client: wallet_client,
            faucet,
        })
    }

    // TODO: figure out a more efficient way to test a local cluster
    // A potential way to do this is to subscribe to txns from fullnode
    // when the feature is ready
    pub async fn let_fullnode_sync(&self, digests: Vec<TransactionDigest>, timeout_sec: u64) {
        let mut futures = FuturesUnordered::new();
        for digest in digests.clone() {
            let task = self.get_tx_with_retry_times(digest, 1);
            futures.push(Box::pin(task));
        }
        let mut sleep = Box::pin(time::sleep(Duration::from_secs(timeout_sec)));

        loop {
            tokio::select! {
                _ = &mut sleep => {
                    panic!("Fullnode does not know all of {:?} after {} secs.", digests, timeout_sec);
                }
                res = futures.next() => {
                    match res {
                        Some((true, _, _)) => {},
                        Some((false, digest, retry_times)) => {
                            let task = self.get_tx_with_retry_times(digest, retry_times);
                            futures.push(Box::pin(task));
                        },
                        None => break, // all txns appear on fullnode, mission completed
                    }
                }
            }
        }
    }

    async fn get_tx_with_retry_times(
        &self,
        digest: TransactionDigest,
        retry_times: u64,
    ) -> (bool, TransactionDigest, u64) {
        match self
            .client
            .get_fullnode_client()
            .read_api()
            .get_transaction_with_options(digest, SuiTransactionBlockResponseOptions::new())
            .await
        {
            Ok(_) => (true, digest, retry_times),
            Err(_) => {
                time::sleep(Duration::from_millis(300 * retry_times)).await;
                (false, digest, retry_times + 1)
            }
        }
    }

    async fn check_owner_and_into_gas_coin(
        &self,
        coin_info: Vec<CoinInfo>,
        owner: SuiAddress,
    ) -> Vec<GasCoin> {
        futures::future::join_all(
            coin_info
                .iter()
                .map(|coin_info| {
                    ObjectChecker::new(coin_info.id)
                        .owner(Owner::AddressOwner(owner))
                        .check_into_gas_coin(self.get_fullnode_client())
                })
                .collect::<Vec<_>>(),
        )
        .await
        .into_iter()
        .collect::<Vec<_>>()
    }
}

struct TestResult {
    name: &'static str,
    passed: bool,
    elapsed: std::time::Duration,
    rpcs_tested: Vec<&'static str>,
}

pub struct TestCase<'a> {
    test_case: Box<dyn TestCaseImpl + 'a>,
}

impl<'a> TestCase<'a> {
    pub fn new(test_case: impl TestCaseImpl + 'a) -> Self {
        TestCase {
            test_case: (Box::new(test_case)),
        }
    }

    async fn run(self, ctx: &mut TestContext) -> TestResult {
        let test_name = self.test_case.name();
        let rpcs = self.test_case.rpcs_tested();
        info!("Running test {}.", test_name);

        // TODO: unwind panic and fail gracefully?

        let start = Instant::now();
        let passed = match self.test_case.run(ctx).await {
            Ok(()) => {
                let elapsed = start.elapsed();
                info!("Test {test_name} succeeded in {elapsed:.1?}.");
                true
            }
            Err(e) => {
                let elapsed = start.elapsed();
                error!("Test {test_name} failed in {elapsed:.1?} with error: {e:#}.");
                false
            }
        };
        TestResult {
            name: test_name,
            passed,
            elapsed: start.elapsed(),
            rpcs_tested: rpcs,
        }
    }
}

#[async_trait]
pub trait TestCaseImpl {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    async fn run(&self, ctx: &mut TestContext) -> Result<(), anyhow::Error>;

    /// RPC methods verified by this test, used for the summary report.
    fn rpcs_tested(&self) -> Vec<&'static str> {
        vec![]
    }
}

pub struct ClusterTest;

impl ClusterTest {
    pub async fn run(options: ClusterTestOpt) {
        let suite_start = Instant::now();
        let mut ctx = TestContext::setup(options)
            .await
            .unwrap_or_else(|e| panic!("Failed to set up TestContext, e: {e}"));

        // TODO: collect tests from each test_case file instead.
        let tests = vec![
            TestCase::new(NativeTransferTest {}),
            TestCase::new(CoinMergeSplitTest {}),
            TestCase::new(SharedCounterTest {}),
            TestCase::new(FullNodeExecuteTransactionTest {}),
            TestCase::new(FullNodeBuildPublishTransactionTest {}),
            TestCase::new(CoinIndexTest {}),
            TestCase::new(RandomBeaconTest {}),
            TestCase::new(PtbTest {}),
            TestCase::new(EventQueryTest {}),
            TestCase::new(PackageUpgradeTest {}),
            TestCase::new(DynamicFieldTest {}),
            TestCase::new(CheckpointTransactionTest {}),
            TestCase::new(GovernanceStakingTest {}),
        ];

        // TODO: improve the runner parallelism for efficiency
        // For now we run tests serially
        let total_cnt = tests.len();
        let mut results: Vec<TestResult> = Vec::with_capacity(total_cnt);
        for t in tests {
            results.push(t.run(&mut ctx).await);
        }
        let suite_elapsed = suite_start.elapsed();

        // Print results summary
        let success_cnt = results.iter().filter(|r| r.passed).count();
        info!("");
        info!("=== Test Results ===");
        for r in &results {
            let status = if r.passed { "PASS" } else { "FAIL" };
            info!("  [{status}] {:40} ({:.1?})", r.name, r.elapsed);
        }
        info!("");
        info!("{success_cnt} of {total_cnt} tests passed in {suite_elapsed:.1?}.");

        // Print RPC coverage summary from passing tests
        let mut verified_rpcs: Vec<&str> = results
            .iter()
            .filter(|r| r.passed)
            .flat_map(|r| r.rpcs_tested.iter().copied())
            .collect();
        verified_rpcs.sort_unstable();
        verified_rpcs.dedup();

        if !verified_rpcs.is_empty() {
            info!("");
            info!("=== Verified RPC Methods ({}) ===", verified_rpcs.len());
            for rpc in &verified_rpcs {
                info!("  {rpc}");
            }
        }

        if success_cnt < total_cnt {
            let failed: Vec<&str> = results
                .iter()
                .filter(|r| !r.passed)
                .map(|r| r.name)
                .collect();
            panic!(
                "{success_cnt} of {total_cnt} tests passed. Failed: {}",
                failed.join(", ")
            );
        }
    }
}
