# Copyright (c) Mysten Labs, Inc.
# SPDX-License-Identifier: Apache-2.0

# This tests the error message when you set your local client to an ephemeral network and then do `sui move build`

sui --client.config $CONFIG client switch --env devnet

echo "== should fail and suggest test-publish or adding env to manifest =="
sui move publish
