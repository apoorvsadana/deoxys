use jsonrpsee::core::{async_trait, RpcResult};
use log::error;
use mc_genesis_data_provider::GenesisProvider;
pub use mc_rpc_core::utils::*;
pub use mc_rpc_core::{Felt, StarknetTraceRpcApiServer, StarknetWriteRpcApiServer, GetClassAtServer};
use mp_contract::class::ContractClassWrapper;
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
use starknet_core::types::{BlockId, ContractClass, FieldElement};

use crate::errors::StarknetRpcApiError;
use crate::Starknet;

#[async_trait]
#[allow(unused_variables)]
impl<A, B, BE, G, C, P, H> GetClassAtServer for Starknet<A, B, BE, G, C, P, H>
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
    /// Get the Contract Class Definition at a Given Address in a Specific Block
    ///
    /// ### Arguments
    ///
    /// * `block_id` - The identifier of the block. This can be the hash of the block, its number
    ///   (height), or a specific block tag.
    /// * `contract_address` - The address of the contract whose class definition will be returned.
    ///
    /// ### Returns
    ///
    /// * `contract_class` - The contract class definition. This may be either a standard contract
    ///   class or a deprecated contract class, depending on the contract's status and the
    ///   blockchain's version.
    ///
    /// ### Errors
    ///
    /// This method may return the following errors:
    /// * `BLOCK_NOT_FOUND` - If the specified block does not exist in the blockchain.
    /// * `CONTRACT_NOT_FOUND` - If the specified contract address does not exist.
    fn get_class_at(&self, block_id: BlockId, contract_address: FieldElement) -> RpcResult<ContractClass> {
        let substrate_block_hash = self.substrate_block_hash_from_starknet_block(block_id).map_err(|e| {
            error!("'{e}'");
            StarknetRpcApiError::BlockNotFound
        })?;

        let contract_address_wrapped = Felt252Wrapper(contract_address).into();

        let contract_class = self
            .overrides
            .for_block_hash(self.client.as_ref(), substrate_block_hash)
            .contract_class_by_address(substrate_block_hash, contract_address_wrapped)
            .ok_or_else(|| {
                error!("Failed to retrieve contract class at '{contract_address}'");
                StarknetRpcApiError::ContractNotFound
            })?;

        // Blockifier classes do not store ABI, has to be retrieved separately
        let contract_abi = self
            .overrides
            .for_block_hash(self.client.as_ref(), substrate_block_hash)
            .contract_abi_by_address(substrate_block_hash, contract_address_wrapped)
            .ok_or_else(|| {
                error!("Failed to retrieve contract ABI at '{contract_address}'");
                StarknetRpcApiError::ContractNotFound
            })?;

        // converting from stored Blockifier class to rpc class
        Ok(ContractClassWrapper { contract: contract_class, abi: contract_abi }.try_into().map_err(|e| {
            log::error!("Failed to convert contract class at address '{contract_address}' to RPC contract class: {e}");
            StarknetRpcApiError::InternalServerError
        })?)
    }
}
