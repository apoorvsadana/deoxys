use jsonrpsee::core::{async_trait, RpcResult};
use log::error;
use mc_genesis_data_provider::GenesisProvider;
pub use mc_rpc_core::utils::*;
use mc_rpc_core::GetStroageAtServer;
pub use mc_rpc_core::{BlockNumberServer, Felt, StarknetTraceRpcApiServer, StarknetWriteRpcApiServer};
use mp_felt::Felt252Wrapper;
use mp_hashers::HasherT;
use pallet_starknet_runtime_api::{ConvertTransactionRuntimeApi, StarknetRuntimeApi};
use sc_client_api::backend::{Backend, StorageProvider};
use sc_client_api::BlockBackend;
use sc_transaction_pool::ChainApi;
use sc_transaction_pool_api::TransactionPool;
use sp_api::ProvideRuntimeApi;
use sp_blockchain::HeaderBackend;
use sp_runtime::traits::Block as BlockT;
use starknet_core::types::{BlockId, FieldElement};

use crate::errors::StarknetRpcApiError;
use crate::Starknet;

#[async_trait]
#[allow(unused_variables)]
impl<A, B, BE, G, C, P, H> GetStroageAtServer for Starknet<A, B, BE, G, C, P, H>
where
    A: ChainApi<Block = B> + 'static,
    B: BlockT,
    P: TransactionPool<Block = B> + 'static,
    BE: Backend<B> + 'static,
    C: HeaderBackend<B> + BlockBackend<B> + StorageProvider<B, BE> + 'static,
    C: ProvideRuntimeApi<B>,
    C::Api: StarknetRuntimeApi<B> + ConvertTransactionRuntimeApi<B>,
    G: GenesisProvider + Send + Sync + 'static,
    H: HasherT + Send + Sync + 'static,
{
    /// Get the value of the storage at the given address and key.
    ///
    /// This function retrieves the value stored in a specified contract's storage, identified by a
    /// contract address and a storage key, within a specified block in the current network.
    ///
    /// ### Arguments
    ///
    /// * `contract_address` - The address of the contract to read from. This parameter identifies
    ///   the contract whose storage is being queried.
    /// * `key` - The key to the storage value for the given contract. This parameter specifies the
    ///   particular storage slot to be queried.
    /// * `block_id` - The hash of the requested block, or number (height) of the requested block,
    ///   or a block tag. This parameter defines the state of the blockchain at which the storage
    ///   value is to be read.
    ///
    /// ### Returns
    ///
    /// Returns the value at the given key for the given contract, represented as a `FieldElement`.
    /// If no value is found at the specified storage key, returns 0.
    ///
    /// ### Errors
    ///
    /// This function may return errors in the following cases:
    ///
    /// * `BLOCK_NOT_FOUND` - If the specified block does not exist in the blockchain.
    /// * `CONTRACT_NOT_FOUND` - If the specified contract does not exist or is not deployed at the
    ///   given `contract_address` in the specified block.
    /// * `STORAGE_KEY_NOT_FOUND` - If the specified storage key does not exist within the given
    ///   contract.
    fn get_storage_at(&self, contract_address: FieldElement, key: FieldElement, block_id: BlockId) -> RpcResult<Felt> {
        let substrate_block_hash = self.substrate_block_hash_from_starknet_block(block_id).map_err(|e| {
            error!("'{e}'");
            StarknetRpcApiError::BlockNotFound
        })?;

        let contract_address = Felt252Wrapper(contract_address).into();
        let key = Felt252Wrapper(key).into();

        let value = self
            .overrides
            .for_block_hash(self.client.as_ref(), substrate_block_hash)
            .get_storage_by_storage_key(substrate_block_hash, contract_address, key)
            .ok_or_else(|| {
                error!("Failed to retrieve storage at '{contract_address:?}' and '{key:?}'");
                StarknetRpcApiError::ContractNotFound
            })?;

        Ok(Felt(Felt252Wrapper::from(value).into()))
    }
}
