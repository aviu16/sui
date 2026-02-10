// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::{TestCaseImpl, TestContext};
use anyhow::Context;
use async_trait::async_trait;
use move_core_types::identifier::Identifier;
use sui_move_build::test_utils::compile_example_package;
use sui_test_transaction_builder::{PublishData, TestTransactionBuilder};
use sui_types::SUI_FRAMEWORK_PACKAGE_ID;
use sui_types::effects::TransactionEffectsAPI;
use sui_types::move_package::UpgradePolicy;
use sui_types::programmable_transaction_builder::ProgrammableTransactionBuilder;
use sui_types::transaction::{ObjectArg, TransactionData};
use tracing::info;

pub struct PackageUpgradeTest;

#[async_trait]
impl TestCaseImpl for PackageUpgradeTest {
    fn name(&self) -> &'static str {
        "PackageUpgrade"
    }

    fn description(&self) -> &'static str {
        "Test publishing a Move package and upgrading it"
    }

    async fn run(&self, ctx: &mut TestContext) -> Result<(), anyhow::Error> {
        info!("Testing package publish and upgrade");

        ctx.get_sui_from_faucet(Some(1)).await;
        let sender = ctx.get_wallet_address();
        let wallet = ctx.get_wallet();
        let rgp = ctx.get_grpc_client().get_reference_gas_price().await?;

        // Step 1: Compile and publish the base package
        info!("Publishing base package (move_upgrade/base)");
        let base_compiled =
            compile_example_package("../../crates/sui-core/src/unit_tests/data/move_upgrade/base")
                .await;

        let gas_obj = wallet
            .get_one_gas_object_owned_by_address(sender)
            .await
            .context("fetching gas object for publish")?
            .expect("Should have a gas object");

        let tx_data = TestTransactionBuilder::new(sender, gas_obj, rgp)
            .publish_with_data(PublishData::CompiledPackage(base_compiled))
            .build();
        let response = ctx
            .grpc_sign_and_execute(tx_data, "publish base package")
            .await;

        let package_ref = response
            .get_new_package_obj()
            .expect("Should find published package");
        let package_id = package_ref.0;

        let upgrade_cap_ref = response
            .get_new_package_upgrade_cap()
            .expect("Should find UpgradeCap");

        info!(
            "Base package published: {}, UpgradeCap: {}",
            package_id, upgrade_cap_ref.0
        );

        let tx_digest = *response.effects.transaction_digest();
        ctx.let_fullnode_sync(vec![tx_digest], 5).await;

        // Step 2: Compile the upgrade package and perform upgrade
        info!("Upgrading to stage1_basic_compatibility_valid");
        let upgrade_compiled = compile_example_package(
            "../../crates/sui-core/src/unit_tests/data/move_upgrade/stage1_basic_compatibility_valid",
        )
        .await;
        let upgrade_modules = upgrade_compiled.get_package_bytes(false);
        let upgrade_digest = upgrade_compiled.get_package_digest(false).to_vec();
        let upgrade_dep_ids = upgrade_compiled.get_published_dependencies_ids();

        // Build the upgrade PTB
        let mut builder = ProgrammableTransactionBuilder::new();

        let cap_arg = builder
            .obj(ObjectArg::ImmOrOwnedObject(upgrade_cap_ref))
            .unwrap();
        let policy_arg = builder.pure(UpgradePolicy::COMPATIBLE).unwrap();
        let digest_arg = builder.pure(upgrade_digest).unwrap();

        // authorize_upgrade(cap, policy, digest) -> UpgradeTicket
        let ticket = builder.programmable_move_call(
            SUI_FRAMEWORK_PACKAGE_ID,
            Identifier::new("package").unwrap(),
            Identifier::new("authorize_upgrade").unwrap(),
            vec![],
            vec![cap_arg, policy_arg, digest_arg],
        );

        // upgrade(current_package, ticket, deps, modules) -> UpgradeReceipt
        let receipt = builder.upgrade(package_id, ticket, upgrade_dep_ids, upgrade_modules);

        // commit_upgrade(cap, receipt)
        builder.programmable_move_call(
            SUI_FRAMEWORK_PACKAGE_ID,
            Identifier::new("package").unwrap(),
            Identifier::new("commit_upgrade").unwrap(),
            vec![],
            vec![cap_arg, receipt],
        );

        let pt = builder.finish();

        // Get a gas object for the upgrade transaction
        let gas_obj = wallet
            .get_one_gas_object_owned_by_address(sender)
            .await
            .context("fetching gas object for upgrade")?
            .expect("Should have a gas object for upgrade");

        let tx_data =
            TransactionData::new_programmable(sender, vec![gas_obj], pt, rgp * 5_000_000, rgp);
        let response = ctx.grpc_sign_and_execute(tx_data, "upgrade package").await;

        // Verify the upgrade created a new package version
        let new_package_ref = response
            .get_new_package_obj()
            .expect("Upgrade should create a new package version");
        let new_package_id = new_package_ref.0;

        info!("Package upgraded: {} -> {}", package_id, new_package_id);

        let tx_digest = *response.effects.transaction_digest();
        ctx.let_fullnode_sync(vec![tx_digest], 5).await;

        // Verify the new package exists on the fullnode via gRPC
        let mut grpc = ctx.get_grpc_client();
        let _new_pkg = grpc
            .get_object(new_package_id)
            .await
            .context("reading upgraded package from fullnode")?;
        info!("Upgraded package verified on fullnode");

        Ok(())
    }
}
