// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::{TestCaseImpl, TestContext};
use anyhow::Context;
use async_trait::async_trait;
use move_core_types::language_storage::TypeTag;
use serde_json::json;
use sui_json::SuiJsonValue;
use sui_json_rpc_types::{ObjectChange, SuiTransactionBlockEffectsAPI};
use sui_move_build::test_utils::compile_example_package;
use sui_types::base_types::ObjectID;
use sui_types::dynamic_field::DynamicFieldName;
use sui_types::object::Owner;
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
        vec![
            "unsafe_publish",
            "unsafe_moveCall",
            "sui_executeTransactionBlock",
            "suix_getDynamicFields",
            "suix_getDynamicFieldObject",
        ]
    }

    async fn run(&self, ctx: &mut TestContext) -> Result<(), anyhow::Error> {
        info!("Testing dynamic field operations");

        ctx.get_sui_from_faucet(Some(1)).await;
        let account = ctx.get_wallet_address();
        let client = ctx.clone_fullnode_client();
        let rgp = ctx.get_reference_gas_price().await;

        // Step 1: Publish the object_basics package
        info!("Publishing object_basics package");
        let compiled =
            compile_example_package("../../crates/sui-core/src/unit_tests/data/object_basics")
                .await;
        let module_bytes = compiled.get_package_base64(false);
        let deps = compiled.get_dependency_storage_package_ids();

        let params = jsonrpsee::rpc_params![
            account,
            module_bytes,
            deps,
            None::<ObjectID>,
            500_000_000u64.to_string()
        ];
        let data = ctx
            .build_transaction_remotely("unsafe_publish", params)
            .await
            .context("building publish transaction for object_basics")?;
        let response = ctx.sign_and_execute(data, "publish object_basics").await;
        let changes = response.object_changes.as_ref().unwrap();

        let package_id = changes
            .iter()
            .find_map(|change| match change {
                ObjectChange::Published { package_id, .. } => Some(*package_id),
                _ => None,
            })
            .expect("Should find published package");

        info!("object_basics published: {}", package_id);
        ctx.let_fullnode_sync(vec![response.digest], 5).await;

        // Step 2: Create a parent object
        info!("Creating parent object");
        let txn = client
            .transaction_builder()
            .move_call(
                account,
                package_id,
                "object_basics",
                "create",
                vec![],
                vec![
                    SuiJsonValue::new(json!("42"))?,
                    SuiJsonValue::new(json!(account))?,
                ],
                None,
                rgp * 2_000_000,
                None,
            )
            .await
            .context("building move_call for create parent")?;
        let response = ctx.sign_and_execute(txn, "create parent object").await;
        let parent_id = response
            .effects
            .as_ref()
            .unwrap()
            .created()
            .iter()
            .find(|o| o.owner == Owner::AddressOwner(account))
            .expect("Should create a parent object")
            .reference
            .object_id;
        ctx.let_fullnode_sync(vec![response.digest], 5).await;
        info!("Parent object created: {}", parent_id);

        // Step 3: Create a child object
        info!("Creating child object");
        let txn = client
            .transaction_builder()
            .move_call(
                account,
                package_id,
                "object_basics",
                "create",
                vec![],
                vec![
                    SuiJsonValue::new(json!("100"))?,
                    SuiJsonValue::new(json!(account))?,
                ],
                None,
                rgp * 2_000_000,
                None,
            )
            .await
            .context("building move_call for create child")?;
        let response = ctx.sign_and_execute(txn, "create child object").await;
        let child_id = response
            .effects
            .as_ref()
            .unwrap()
            .created()
            .iter()
            .find(|o| o.owner == Owner::AddressOwner(account))
            .expect("Should create a child object")
            .reference
            .object_id;
        ctx.let_fullnode_sync(vec![response.digest], 5).await;
        info!("Child object created: {}", child_id);

        // Step 4: Add the child as a dynamic object field on the parent
        info!("Adding dynamic object field");
        let txn = client
            .transaction_builder()
            .move_call(
                account,
                package_id,
                "object_basics",
                "add_ofield",
                vec![],
                vec![
                    SuiJsonValue::from_object_id(parent_id),
                    SuiJsonValue::from_object_id(child_id),
                ],
                None,
                rgp * 2_000_000,
                None,
            )
            .await
            .context("building move_call for add_ofield")?;
        let response = ctx.sign_and_execute(txn, "add dynamic object field").await;
        assert!(response.status_ok().unwrap());
        ctx.let_fullnode_sync(vec![response.digest], 5).await;
        info!(
            "Dynamic object field added: child {} on parent {}",
            child_id, parent_id
        );

        // Step 5: Query dynamic fields via RPC
        info!("Testing get_dynamic_fields RPC");
        let dynamic_fields = client
            .read_api()
            .get_dynamic_fields(parent_id, None, Some(10))
            .await
            .context("get_dynamic_fields")?;
        assert!(
            !dynamic_fields.data.is_empty(),
            "Parent should have at least one dynamic field"
        );
        info!(
            "get_dynamic_fields verified: {} field(s) on parent",
            dynamic_fields.data.len()
        );

        // Step 6: Query dynamic field object via RPC
        info!("Testing get_dynamic_field_object RPC");
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
        let txn = client
            .transaction_builder()
            .move_call(
                account,
                package_id,
                "object_basics",
                "remove_ofield",
                vec![],
                vec![SuiJsonValue::from_object_id(parent_id)],
                None,
                rgp * 2_000_000,
                None,
            )
            .await
            .context("building move_call for remove_ofield")?;
        let response = ctx
            .sign_and_execute(txn, "remove dynamic object field")
            .await;
        assert!(response.status_ok().unwrap());
        ctx.let_fullnode_sync(vec![response.digest], 5).await;

        // Verify the dynamic field is gone
        let dynamic_fields_after = client
            .read_api()
            .get_dynamic_fields(parent_id, None, Some(10))
            .await
            .context("get_dynamic_fields after removal")?;
        assert!(
            dynamic_fields_after.data.is_empty(),
            "Parent should have no dynamic fields after removal"
        );
        info!("Dynamic field removal verified: 0 fields remaining");

        Ok(())
    }
}
