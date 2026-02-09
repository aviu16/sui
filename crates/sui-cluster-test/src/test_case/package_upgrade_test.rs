// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::{TestCaseImpl, TestContext};
use async_trait::async_trait;
use jsonrpsee::rpc_params;
use move_core_types::identifier::Identifier;
use sui_json_rpc_types::ObjectChange;
use sui_move_build::test_utils::compile_example_package;
use sui_types::SUI_FRAMEWORK_PACKAGE_ID;
use sui_types::base_types::ObjectID;
use sui_types::move_package::UpgradePolicy;
use sui_types::object::Owner;
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
        let rgp = ctx.get_reference_gas_price().await;

        // Step 1: Compile and publish the base package
        info!("Publishing base package (move_upgrade/base)");
        let base_compiled =
            compile_example_package("../../crates/sui-core/src/unit_tests/data/move_upgrade/base")
                .await;
        let base_module_bytes =
            base_compiled.get_package_base64(/* with_unpublished_deps */ false);
        let base_deps = base_compiled.get_dependency_storage_package_ids();

        let params = rpc_params![
            sender,
            base_module_bytes,
            base_deps,
            None::<ObjectID>,
            500_000_000u64.to_string()
        ];
        let data = ctx
            .build_transaction_remotely("unsafe_publish", params)
            .await?;
        let response = ctx.sign_and_execute(data, "publish base package").await;
        let changes = response.object_changes.as_ref().unwrap();

        // Find the published package
        let package_id = changes
            .iter()
            .find_map(|change| match change {
                ObjectChange::Published { package_id, .. } => Some(*package_id),
                _ => None,
            })
            .expect("Should find published package");

        // Find the UpgradeCap (owned object with type UpgradeCap)
        let upgrade_cap_ref = changes
            .iter()
            .find_map(|change| match change {
                ObjectChange::Created {
                    owner: Owner::AddressOwner(_),
                    object_type,
                    object_id,
                    version,
                    digest,
                    ..
                } if object_type.name.as_str() == "UpgradeCap" => {
                    Some((*object_id, *version, *digest))
                }
                _ => None,
            })
            .expect("Should find UpgradeCap");

        info!(
            "Base package published: {}, UpgradeCap: {}",
            package_id, upgrade_cap_ref.0
        );

        ctx.let_fullnode_sync(vec![response.digest], 5).await;

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
        let wallet = ctx.get_wallet();
        let gas_obj = wallet
            .get_one_gas_object_owned_by_address(sender)
            .await?
            .expect("Should have a gas object for upgrade");

        let tx_data =
            TransactionData::new_programmable(sender, vec![gas_obj], pt, rgp * 5_000_000, rgp);
        let response = ctx.sign_and_execute(tx_data, "upgrade package").await;

        // Verify the upgrade created a new package version
        let upgrade_changes = response.object_changes.as_ref().unwrap();
        let new_package_id = upgrade_changes
            .iter()
            .find_map(|change| match change {
                ObjectChange::Published { package_id, .. } => Some(*package_id),
                _ => None,
            })
            .expect("Upgrade should create a new package version");

        info!(
            "Package upgraded successfully. New package: {}",
            new_package_id
        );

        ctx.let_fullnode_sync(vec![response.digest], 5).await;

        // Verify the new package exists on the fullnode
        let new_pkg_obj = ctx
            .get_fullnode_client()
            .read_api()
            .get_object_with_options(
                new_package_id,
                sui_json_rpc_types::SuiObjectDataOptions::new().with_owner(),
            )
            .await
            .expect("Should be able to read upgraded package");
        assert!(
            new_pkg_obj.data.is_some(),
            "Upgraded package should exist on fullnode"
        );

        Ok(())
    }
}
