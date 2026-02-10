// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::{TestCaseImpl, TestContext};
use anyhow::Context;
use async_trait::async_trait;
use move_core_types::language_storage::TypeTag;
use serde_json::json;
use sui_move_build::test_utils::compile_example_package;
use sui_test_transaction_builder::{PublishData, TestTransactionBuilder};
use sui_types::dynamic_field::DynamicFieldName;
use sui_types::effects::TransactionEffectsAPI;
use sui_types::object::Owner;
use sui_types::transaction::{CallArg, ObjectArg};
use tracing::info;

pub struct DynamicFieldTest;

#[async_trait]
impl TestCaseImpl for DynamicFieldTest {
    fn name(&self) -> &'static str {
        "DynamicField"
    }

    fn description(&self) -> &'static str {
        "Test dynamic field operations and RPC queries"
    }

    fn rpcs_tested(&self) -> Vec<&'static str> {
        vec!["suix_getDynamicFieldObject"]
    }

    async fn run(&self, ctx: &mut TestContext) -> Result<(), anyhow::Error> {
        info!("Testing dynamic field operations");

        ctx.get_sui_from_faucet(Some(1)).await;
        let account = ctx.get_wallet_address();
        let wallet = ctx.get_wallet();
        let rgp = ctx.get_grpc_client().get_reference_gas_price().await?;

        // Step 1: Publish the object_basics package
        info!("Publishing object_basics package");
        let compiled =
            compile_example_package("../../crates/sui-core/src/unit_tests/data/object_basics")
                .await;

        let gas_obj = wallet
            .get_one_gas_object_owned_by_address(account)
            .await
            .context("fetching gas object for publish")?
            .expect("Should have a gas object");

        let tx_data = TestTransactionBuilder::new(account, gas_obj, rgp)
            .publish_with_data(PublishData::CompiledPackage(compiled))
            .build();
        let response = ctx
            .grpc_sign_and_execute(tx_data, "publish object_basics")
            .await;

        let package_id = response
            .get_new_package_obj()
            .expect("Should find published package")
            .0;

        info!("object_basics published: {}", package_id);
        let tx_digest = *response.effects.transaction_digest();
        ctx.let_fullnode_sync(vec![tx_digest], 5).await;

        // Step 2: Create a parent object
        info!("Creating parent object");
        let gas_obj = wallet
            .get_one_gas_object_owned_by_address(account)
            .await
            .context("fetching gas object for create parent")?
            .expect("Should have a gas object");

        let tx_data = TestTransactionBuilder::new(account, gas_obj, rgp)
            .move_call(
                package_id,
                "object_basics",
                "create",
                vec![
                    CallArg::Pure(bcs::to_bytes(&42u64).unwrap()),
                    CallArg::Pure(bcs::to_bytes(&account).unwrap()),
                ],
            )
            .build();
        let response = ctx
            .grpc_sign_and_execute(tx_data, "create parent object")
            .await;

        let parent_id = response
            .effects
            .created()
            .iter()
            .find(|o| o.1 == Owner::AddressOwner(account))
            .expect("Should create a parent object")
            .0
            .0;
        let tx_digest = *response.effects.transaction_digest();
        ctx.let_fullnode_sync(vec![tx_digest], 5).await;
        info!("Parent object created: {}", parent_id);

        // Step 3: Create a child object
        info!("Creating child object");
        let gas_obj = wallet
            .get_one_gas_object_owned_by_address(account)
            .await
            .context("fetching gas object for create child")?
            .expect("Should have a gas object");

        let tx_data = TestTransactionBuilder::new(account, gas_obj, rgp)
            .move_call(
                package_id,
                "object_basics",
                "create",
                vec![
                    CallArg::Pure(bcs::to_bytes(&100u64).unwrap()),
                    CallArg::Pure(bcs::to_bytes(&account).unwrap()),
                ],
            )
            .build();
        let response = ctx
            .grpc_sign_and_execute(tx_data, "create child object")
            .await;

        let child_id = response
            .effects
            .created()
            .iter()
            .find(|o| o.1 == Owner::AddressOwner(account))
            .expect("Should create a child object")
            .0
            .0;
        let tx_digest = *response.effects.transaction_digest();
        ctx.let_fullnode_sync(vec![tx_digest], 5).await;
        info!("Child object created: {}", child_id);

        // Step 4: Add the child as a dynamic object field on the parent
        info!("Adding dynamic object field");
        let mut grpc = ctx.get_grpc_client();
        let parent_obj = grpc
            .get_object(parent_id)
            .await
            .context("fetching parent object")?;
        let parent_ref = parent_obj.compute_object_reference();
        let child_obj = grpc
            .get_object(child_id)
            .await
            .context("fetching child object")?;
        let child_ref = child_obj.compute_object_reference();

        let gas_obj = wallet
            .get_one_gas_object_owned_by_address(account)
            .await
            .context("fetching gas object for add_ofield")?
            .expect("Should have a gas object");

        let tx_data = TestTransactionBuilder::new(account, gas_obj, rgp)
            .move_call(
                package_id,
                "object_basics",
                "add_ofield",
                vec![
                    CallArg::Object(ObjectArg::ImmOrOwnedObject(parent_ref)),
                    CallArg::Object(ObjectArg::ImmOrOwnedObject(child_ref)),
                ],
            )
            .build();
        let response = ctx
            .grpc_sign_and_execute(tx_data, "add dynamic object field")
            .await;
        assert!(response.effects.status().is_ok());
        let tx_digest = *response.effects.transaction_digest();
        ctx.let_fullnode_sync(vec![tx_digest], 5).await;
        info!(
            "Dynamic object field added: child {} on parent {}",
            child_id, parent_id
        );

        // Step 5: Query dynamic fields via gRPC
        info!("Testing get_dynamic_fields via gRPC");
        let dynamic_fields = grpc
            .get_dynamic_fields(parent_id, Some(10), None)
            .await
            .context("get_dynamic_fields")?;
        assert!(
            !dynamic_fields.dynamic_fields.is_empty(),
            "Parent should have at least one dynamic field"
        );
        info!(
            "get_dynamic_fields verified: {} field(s) on parent",
            dynamic_fields.dynamic_fields.len()
        );

        // Step 6: Query dynamic field object via JSON-RPC (no gRPC equivalent)
        info!("Testing get_dynamic_field_object RPC");
        let client = ctx.clone_fullnode_client();
        let field_name = DynamicFieldName {
            type_: TypeTag::Bool,
            value: json!(true),
        };
        let field_obj = client
            .read_api()
            .get_dynamic_field_object(parent_id, field_name)
            .await
            .context("get_dynamic_field_object")?;
        assert!(
            field_obj.data.is_some(),
            "Dynamic field object should exist"
        );
        info!("get_dynamic_field_object verified: field exists");

        // Step 7: Remove the dynamic object field
        info!("Removing dynamic object field");
        let parent_obj = grpc
            .get_object(parent_id)
            .await
            .context("fetching parent object for remove")?;
        let parent_ref = parent_obj.compute_object_reference();

        let gas_obj = wallet
            .get_one_gas_object_owned_by_address(account)
            .await
            .context("fetching gas object for remove_ofield")?
            .expect("Should have a gas object");

        let tx_data = TestTransactionBuilder::new(account, gas_obj, rgp)
            .move_call(
                package_id,
                "object_basics",
                "remove_ofield",
                vec![CallArg::Object(ObjectArg::ImmOrOwnedObject(parent_ref))],
            )
            .build();
        let response = ctx
            .grpc_sign_and_execute(tx_data, "remove dynamic object field")
            .await;
        assert!(response.effects.status().is_ok());
        let tx_digest = *response.effects.transaction_digest();
        ctx.let_fullnode_sync(vec![tx_digest], 5).await;

        // Verify the dynamic field is gone via gRPC
        let dynamic_fields_after = grpc
            .get_dynamic_fields(parent_id, Some(10), None)
            .await
            .context("get_dynamic_fields after removal")?;
        assert!(
            dynamic_fields_after.dynamic_fields.is_empty(),
            "Parent should have no dynamic fields after removal"
        );
        info!("Dynamic field removal verified: 0 fields remaining");

        Ok(())
    }
}
