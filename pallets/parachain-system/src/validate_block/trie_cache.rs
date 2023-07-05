// This file is part of Substrate.

// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use sp_std::cell::{RefCell, RefMut};
use hash_db::{HashDB, Hasher};
use hashbrown::hash_map::{Entry, HashMap};
use sp_state_machine::{TrieBackendStorage, TrieCacheProvider};
use sp_std::boxed::Box;
use sp_trie::NodeCodec;
use trie_db::{node::NodeOwned, TrieCache, TrieError};

/// Special purpose trie cache implementation that is able to cache an unlimited number
/// of values. To be used in `validate_block` to serve values and nodes that
/// have already been loaded and decoded from the storage proof.
pub(crate) struct SimpleTrieCache<'a, H: Hasher> {
	node_cache: core::cell::RefMut<'a, HashMap<H::Out, NodeOwned<H::Out>>>,
	value_cache: core::cell::RefMut<'a, HashMap<Box<[u8]>, trie_db::CachedValue<H::Out>>>,
}

impl<'a, H: Hasher> trie_db::TrieCache<NodeCodec<H>> for SimpleTrieCache<'a, H> {
	fn lookup_value_for_key(&mut self, key: &[u8]) -> Option<&trie_db::CachedValue<H::Out>> {
		self.value_cache.get(key)
	}

	fn cache_value_for_key(&mut self, key: &[u8], value: trie_db::CachedValue<H::Out>) {
		self.value_cache.insert(key.into(), value);
	}

	fn get_or_insert_node(
		&mut self,
		hash: <NodeCodec<H> as trie_db::NodeCodec>::HashOut,
		fetch_node: &mut dyn FnMut() -> trie_db::Result<
			NodeOwned<H::Out>,
			H::Out,
			<NodeCodec<H> as trie_db::NodeCodec>::Error,
		>,
	) -> trie_db::Result<&NodeOwned<H::Out>, H::Out, <NodeCodec<H> as trie_db::NodeCodec>::Error> {
		match self.node_cache.entry(hash) {
			Entry::Occupied(entry) => Ok(entry.into_mut()),
			Entry::Vacant(entry) => {
				Ok(entry.insert(fetch_node()?))
			},
		}
	}

	fn get_node(
		&mut self,
		hash: &H::Out,
	) -> Option<&NodeOwned<<NodeCodec<H> as trie_db::NodeCodec>::HashOut>> {
		self.node_cache.get(hash)
	}
}

/// Provider of [`SimpleTrieCache`] instances.
pub(crate) struct CacheProvider<H: Hasher> {
	node_cache: RefCell<HashMap<H::Out, NodeOwned<H::Out>>>,
	value_cache: RefCell<HashMap<Box<[u8]>, trie_db::CachedValue<H::Out>>>,
}

impl<H: Hasher> CacheProvider<H> {
	/// Constructs a new instance of CacheProvider with an uninitialized state
	/// and empty node and value caches.
	pub fn new() -> Self {
		CacheProvider { node_cache: Default::default(), value_cache: Default::default() }
	}
}

impl<H: Hasher> TrieCacheProvider<H> for CacheProvider<H> {
	type Cache<'a> = SimpleTrieCache<'a, H> where H: 'a;

	fn as_trie_db_cache(&self, _storage_root: <H as Hasher>::Out) -> Self::Cache<'_> {
		SimpleTrieCache {
			value_cache: self.value_cache.borrow_mut(),
			node_cache: self.node_cache.borrow_mut(),
		}
	}

	fn as_trie_db_mut_cache(&self) -> Self::Cache<'_> {
		SimpleTrieCache {
			value_cache: self.value_cache.borrow_mut(),
			node_cache: self.node_cache.borrow_mut(),
		}
	}

	fn merge<'a>(&'a self, _other: Self::Cache<'a>, _new_root: <H as Hasher>::Out) {}
}

// This is safe here since we are single-threaded in WASM
unsafe impl<H: Hasher> Send for CacheProvider<H> {}
unsafe impl<H: Hasher> Sync for CacheProvider<H> {}
