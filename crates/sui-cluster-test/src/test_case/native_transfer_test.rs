// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use async_trait::async_trait;
use jsonrpsee::rpc_params;
use tracing::info;

use sui_json_rpc_types::{
    SuiExecutionStatus, SuiObjectDataOptions, SuiTransactionBlockEffectsAPI,
    SuiTransactionBlockResponse,
};
use sui_types::{
    base_types::{ObjectID, SuiAddress},
    crypto::{AccountKeyPair, get_key_pair},
    object::Owner,
    transaction::TransactionDataAPI,
};

use crate::{
    TestCaseImpl, TestContext,
    helper::{BalanceChangeChecker, ObjectChecker},
};

pub struct NativeTransferTest;

#[async_trait]
impl TestCaseImpl for NativeTransferTest {
    fn name(&self) -> &'static str {
        "NativeTransfer"
    }

    fn description(&self) -> &'static str {
        "Test tranferring SUI coins natively"
    }

    fn rpcs_tested(&self) -> Vec<&'static str> {
        vec![
            "sui_dryRunTransactionBlock",
            "sui_devInspectTransactionBlock",
            "unsafe_transferObject",
            "unsafe_transferSui",
            "sui_executeTransactionBlock",
            "sui_getObject",
        ]
    }

    async fn run(&self, ctx: &mut TestContext) -> Result<(), anyhow::Error> {
        info!("Testing gas coin transfer");
        let mut sui_objs = ctx.get_sui_from_faucet(Some(1)).await;
        let gas_obj = ctx.get_sui_from_faucet(Some(1)).await.swap_remove(0);

        let signer = ctx.get_wallet_address();
        let (recipient_addr, _): (_, AccountKeyPair) = get_key_pair();
        // Test transfer object
        let obj_to_transfer: ObjectID = *sui_objs.swap_remove(0).id();
        let params = rpc_params![
            signer,
            obj_to_transfer,
            Some(*gas_obj.id()),
            (2_000_000).to_string(),
            recipient_addr
        ];
        let data = ctx
            .build_transaction_remotely("unsafe_transferObject", params)
            .await
            .context("building transferObject transaction")?;

        // Test sui_dryRunTransactionBlock
        info!("Testing dry run of transfer transaction");
        let dry_run_result = ctx
            .get_fullnode_client()
            .read_api()
            .dry_run_transaction_block(data.clone())
            .await
            .context("dry run of transfer transaction")?;
        assert!(
            matches!(dry_run_result.effects.status(), SuiExecutionStatus::Success),
            "Dry run of transfer should succeed, got: {:?}",
            dry_run_result.effects.status()
        );
        assert!(
            !dry_run_result.balance_changes.is_empty(),
            "Dry run should report balance changes for a transfer"
        );
        info!(
            "Dry run verified: status=Success, {} balance change(s)",
            dry_run_result.balance_changes.len()
        );

        // Test sui_devInspectTransactionBlock
        info!("Testing dev inspect of transfer transaction");
        let tx_kind = data.clone().into_kind();
        let dev_inspect_result = ctx
            .get_fullnode_client()
            .read_api()
            .dev_inspect_transaction_block(signer, tx_kind, None, None, None)
            .await
            .context("dev inspect of transfer transaction")?;
        assert!(
            matches!(
                dev_inspect_result.effects.status(),
                SuiExecutionStatus::Success
            ),
            "Dev inspect of transfer should succeed, got: {:?}",
            dev_inspect_result.effects.status()
        );
        info!("Dev inspect verified: status=Success");

        let mut response = ctx.sign_and_execute(data, "coin transfer").await;

        Self::examine_response(ctx, &mut response, signer, recipient_addr, obj_to_transfer).await;
        info!("Transfer object verified: object moved to recipient");

        let mut sui_objs_2 = ctx.get_sui_from_faucet(Some(1)).await;
        // Test transfer sui
        let obj_to_transfer_2 = *sui_objs_2.swap_remove(0).id();
        let params = rpc_params![
            signer,
            obj_to_transfer_2,
            (2_000_000).to_string(),
            recipient_addr,
            None::<u64>
        ];
        let data = ctx
            .build_transaction_remotely("unsafe_transferSui", params)
            .await
            .context("building transferSui transaction")?;
        let mut response = ctx.sign_and_execute(data, "coin transfer").await;

        Self::examine_response(ctx, &mut response, signer, recipient_addr, obj_to_transfer).await;
        info!("Transfer SUI verified: coin moved to recipient");

        // Test error path: non-existent object
        info!("Testing error path: non-existent object");
        let random_id = ObjectID::random();
        let obj_response = ctx
            .get_fullnode_client()
            .read_api()
            .get_object_with_options(random_id, SuiObjectDataOptions::new())
            .await
            .context("get_object for non-existent object")?;
        assert!(
            obj_response.data.is_none(),
            "Non-existent object should have no data"
        );
        assert!(
            obj_response.error.is_some(),
            "Non-existent object should have an error field"
        );
        info!("Error path verified: non-existent object returns error field");

        Ok(())
    }
}

impl NativeTransferTest {
    async fn examine_response(
        ctx: &TestContext,
        response: &mut SuiTransactionBlockResponse,
        signer: SuiAddress,
        recipient: SuiAddress,
        obj_to_transfer_id: ObjectID,
    ) {
        let balance_changes = &mut response.balance_changes.as_mut().unwrap();
        // for transfer we only expect 2 balance changes, one for sender and one for recipient.
        assert_eq!(
            balance_changes.len(),
            2,
            "Expect 2 balance changes emitted, but got {}",
            balance_changes.len()
        );
        // Order of balance change is not fixed so need to check who's balance come first.
        // this make sure recipient always come first
        if balance_changes[0].owner.get_owner_address().unwrap() == signer {
            balance_changes.reverse()
        }
        BalanceChangeChecker::new()
            .owner(Owner::AddressOwner(recipient))
            .coin_type("0x2::sui::SUI")
            .check(&balance_changes.remove(0));
        BalanceChangeChecker::new()
            .owner(Owner::AddressOwner(signer))
            .coin_type("0x2::sui::SUI")
            .check(&balance_changes.remove(0));
        // Verify fullnode observes the txn
        ctx.let_fullnode_sync(vec![response.digest], 5).await;

        let _ = ObjectChecker::new(obj_to_transfer_id)
            .owner(Owner::AddressOwner(recipient))
            .check(ctx.get_fullnode_client())
            .await;
    }
}
