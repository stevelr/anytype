// Standalone Go program to generate BIP39 → Anytype key derivation test vectors.
// Uses the any-sync library as the reference implementation.
//
// Usage:
//   cd anyr/testdata/go-testvec
//   go run .
//
// SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"encoding/base64"
	"fmt"

	"github.com/anyproto/any-sync/util/crypto"
)

func main() {
	mnemonic := crypto.Mnemonic("tag volcano eight thank tide danger coast health above argue embrace heavy")

	masterNode, err := mnemonic.DeriveMasterNode(0)
	if err != nil {
		panic(err)
	}
	nodeBytes, err := masterNode.MarshalBinary()
	if err != nil {
		panic(err)
	}

	res, err := mnemonic.DeriveKeys(0)
	if err != nil {
		panic(err)
	}

	fmt.Println("account_key:", base64.StdEncoding.EncodeToString(nodeBytes))
	fmt.Println("account_id: ", res.Identity.GetPublic().Account())
}
