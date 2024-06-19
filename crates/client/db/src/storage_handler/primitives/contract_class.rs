use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::macos::raw;
use std::sync::Arc;

use anyhow::{anyhow, Context};
use blockifier::execution::contract_class::{
    self, ContractClass as ContractClassBlockifier, ContractClassV0, ContractClassV0Inner, ContractClassV1, EntryPointV1
};
use cairo_vm::types::program::Program;
use dp_convert::to_felt::ToFelt;
use dp_transactions::from_broadcasted_transactions::flattened_sierra_to_casm_contract_class;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use indexmap::IndexMap;
use parity_scale_codec::{Decode, Encode};
use starknet_api::core::{ClassHash, EntryPointSelector, Nonce};
use starknet_api::deprecated_contract_class::{EntryPoint, EntryPointOffset, EntryPointType};
use starknet_api::hash::StarkFelt;
use starknet_core::types::contract::legacy::{
    LegacyContractClass, LegacyEntrypointOffset, RawLegacyAbiEntry, RawLegacyEntryPoint, RawLegacyEntryPoints,
    RawLegacyEvent, RawLegacyFunction, RawLegacyMember, RawLegacyStruct,
};
use starknet_core::types::{ContractClass as ContractClassCore, CompressedLegacyContractClass, EntryPointsByType, FlattenedSierraClass, LegacyContractEntryPoint, LegacyEntryPointsByType, SierraEntryPoint};

#[derive(Debug, Encode, Decode)]
pub struct StorageContractClassData {
    pub contract_class: ContractClassBlockifier,
    pub abi: ContractAbi,
    pub sierra_program_length: u64,
    pub abi_length: u64,
    pub block_number: u64,
}
#[derive(Debug, Clone, Encode, Decode)]
pub struct StorageContractData {
    pub class_hash: ClassHash,
    pub nonce: Nonce,
}

#[derive(Debug, Clone, Encode, Decode)]
pub struct ClassUpdateWrapper(pub Vec<ContractClassData>);
#[derive(Debug, Clone, Encode, Decode)]
pub struct ContractClassData {
    pub hash: ClassHash,
    pub contract_class: ContractClassWrapper,
}

#[derive(Debug, Clone, Encode, Decode)]
pub struct ContractClassWrapper {
    pub contract: ContractClassBlockifier,
    pub abi: ContractAbi,
    pub sierra_program_length: u64,
    pub abi_length: u64,
}
// TODO: move this somewhere more sensible? Would be a good idea to decouple
// publicly available storage data from wrapper classes
#[derive(Debug, Clone, Encode, Decode)]
pub enum ContractAbi {
    Sierra(String),
    Cairo(Option<String>),
}

impl ContractAbi {
    pub fn length(&self) -> usize {
        match self {
            ContractAbi::Sierra(abi) => abi.len(),
            ContractAbi::Cairo(Some(entries)) => entries.len(),
            ContractAbi::Cairo(None) => 0,
        }
    }
}

#[derive(Debug, Clone, Encode, Decode)]
pub enum AbiEntryWrapper {
    Function(AbiFunctionEntryWrapper),
    Event(AbiEventEntryWrapper),
    Struct(AbiStructEntryWrapper),
}

#[derive(Debug, Clone, Encode, Decode)]
pub struct AbiFunctionEntryWrapper {
    /// The function name
    pub name: String,
    /// Typed parameter
    pub inputs: Vec<AbiTypedParameterWrapper>,
    /// Typed parameter
    pub outputs: Vec<AbiTypedParameterWrapper>,
    /// Function state mutability
    pub state_mutability: Option<AbiFunctionStateMutabilityWrapper>,
}

#[derive(Debug, Clone, Encode, Decode)]
pub struct AbiEventEntryWrapper {
    /// The event name
    pub name: String,
    /// Typed parameter
    pub keys: Vec<AbiTypedParameterWrapper>,
    /// Typed parameter
    pub data: Vec<AbiTypedParameterWrapper>,
}

#[derive(Debug, Clone, Encode, Decode)]
pub struct AbiStructEntryWrapper {
    pub name: String,
    pub size: u64,
    pub members: Vec<AbiStructMemberWrapper>,
}

#[derive(Debug, Clone, Encode, Decode)]
pub struct AbiStructMemberWrapper {
    /// The parameter's name
    pub name: String,
    /// The parameter's type
    pub r#type: String,
    /// Offset of this property within the struct
    pub offset: u64,
}

#[derive(Debug, Clone, Encode, Decode)]
pub enum AbiFunctionTypeWrapper {
    Function,
    L1handler,
    Constructor,
}

#[derive(Debug, Clone, Encode, Decode)]
pub enum AbiEventTypeWrapper {
    Event,
}

#[derive(Debug, Clone, Encode, Decode)]
pub enum AbiStructTypeWrapper {
    Struct,
}

#[derive(Debug, Clone, Encode, Decode)]
pub enum AbiFunctionStateMutabilityWrapper {
    View,
}

#[derive(Debug, Clone, Encode, Decode)]
pub struct AbiTypedParameterWrapper {
    pub name: String,
    pub r#type: String,
}

/// Converts a [serde_json::Value] to a [ContractClassBlockifier]
pub fn from_rpc_contract_class(contract_class: &serde_json::Value) -> anyhow::Result<ContractClassBlockifier> {
    if contract_class.get("sierra_program").is_some() {
        from_contract_class_sierra(contract_class)
    } else {
        from_contract_class_cairo(contract_class)
    }
}

/// Converts a [ContractClassBlockifier] to a [ContractClassCore]
///
/// This is after extracting the contract classes.
pub fn to_contract_class_sierra(sierra_class: &ContractClassV1, abi: String) -> anyhow::Result<ContractClassCore> {
    let entry_points_by_type: HashMap<_, _> =
        sierra_class.entry_points_by_type.iter().map(|(k, v)| (*k, v.clone())).collect();
    let entry_points_by_type = to_entry_points_by_type(&entry_points_by_type)?;

    let sierra_program = sierra_class
        .program
        .iter_data()
        .filter_map(|maybe_relocatable| {
            maybe_relocatable.get_int_ref().map(|felt| Felt::from_bytes_be(&((*felt).to_be_bytes())))
        })
        .collect::<Vec<Felt>>();

    Ok(ContractClassCore::Sierra(FlattenedSierraClass {
        sierra_program,
        contract_class_version: "0.1.0".to_string(),
        entry_points_by_type,
        abi,
    }))
}

/// Converts a [FlattenedSierraClass] to a [ContractClassBlockifier]
///
/// This is used before storing the contract classes.
pub fn from_contract_class_sierra(contract_class: &serde_json::Value) -> anyhow::Result<ContractClassBlockifier> {
    let raw_contract_class = serde_json::to_string(contract_class).context("serializing contract class")?;
    let blockifier_contract = ContractClassV1::try_from_json_string(&raw_contract_class).context("converting contract class")?;
    anyhow::Ok(ContractClassBlockifier::V1(blockifier_contract))
}

pub fn to_contract_class_cairo(
    contract_class: &ContractClassV0,
    abi: Option<String>,
) -> anyhow::Result<ContractClassCore> {
    let serialized_program = contract_class.program.serialize().context("serializing program")?;
    let entry_points_by_type: HashMap<_, _> = contract_class.entry_points_by_type.clone().into_iter().collect();
    let serialized_entry_points = to_legacy_entry_points_by_type(&entry_points_by_type)?;

    let serialized_abi = serde_json::from_str(&abi.unwrap()).context("deserializing abi")?;

    let compressed_legacy_contract_class = CompressedLegacyContractClass {
        program: serialized_program,
        entry_points_by_type: serialized_entry_points,
        abi: serialized_abi,
    };

    Ok(ContractClassCore::Legacy(compressed_legacy_contract_class))
}

/// Converts a [serde_json::Value] to a [ContractClassBlockifier]
pub fn from_contract_class_cairo(contract_class: &serde_json::Value) -> anyhow::Result<ContractClassBlockifier> {
    let raw_contract_class = serde_json::to_string(contract_class).context("serializing contract class")?;
    let blockifier_contract = ContractClassV0::try_from_json_string(&raw_contract_class).context("converting contract class")?;
    anyhow::Ok(ContractClassBlockifier::V0(blockifier_contract))
}

/// Returns a compressed vector of bytes
pub(crate) fn compress(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut gzip_encoder = GzEncoder::new(Vec::new(), flate2::Compression::fast());
    // 2023-08-22: JSON serialization is already done in Blockifier
    // https://github.com/keep-starknet-strange/blockifier/blob/no_std-support-7578442/crates/blockifier/src/execution/contract_class.rs#L129
    // https://github.com/keep-starknet-strange/blockifier/blob/no_std-support-7578442/crates/blockifier/src/execution/contract_class.rs#L389
    // serde_json::to_writer(&mut gzip_encoder, data)?;
    gzip_encoder.write_all(data)?;
    Ok(gzip_encoder.finish()?)
}

/// Decompresses a compressed json string into it's byte representation.
/// Example compression from [Starknet-rs](https://github.com/xJonathanLEI/starknet-rs/blob/49719f49a18f9621fc37342959e84900b600083e/starknet-core/src/types/contract/legacy.rs#L473)
pub(crate) fn decompress(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut gzip_decoder = GzDecoder::new(data);
    let mut buf = Vec::<u8>::new();
    gzip_decoder.read_to_end(&mut buf)?;
    anyhow::Ok(buf)
}

/// Returns a [anyhow::Result<LegacyEntryPointsByType>] (starknet-rs type) from
/// a [HashMap<EntryPointType, Vec<EntryPoint>>]
fn to_legacy_entry_points_by_type(
    entries: &HashMap<EntryPointType, Vec<EntryPoint>>,
) -> anyhow::Result<LegacyEntryPointsByType> {
    fn collect_entry_points(
        entries: &HashMap<EntryPointType, Vec<EntryPoint>>,
        entry_point_type: EntryPointType,
    ) -> anyhow::Result<Vec<LegacyContractEntryPoint>> {
        Ok(entries
            .get(&entry_point_type)
            .ok_or(anyhow!("Missing {:?} entry point", entry_point_type))?
            .iter()
            .map(|e| to_legacy_entry_point(e.clone()))
            .collect())
    }

    let constructor = collect_entry_points(entries, EntryPointType::Constructor).unwrap_or_default();
    let external = collect_entry_points(entries, EntryPointType::External)?;
    let l1_handler = collect_entry_points(entries, EntryPointType::L1Handler).unwrap_or_default();

    Ok(LegacyEntryPointsByType { constructor, external, l1_handler })
}

/// Returns a [anyhow::Result<LegacyEntryPointsByType>] (starknet-rs type) from
/// a [HashMap<EntryPointType, Vec<EntryPoinV1>>]
fn to_entry_points_by_type(entries: &HashMap<EntryPointType, Vec<EntryPointV1>>) -> anyhow::Result<EntryPointsByType> {
    fn collect_entry_points(
        entries: &HashMap<EntryPointType, Vec<EntryPointV1>>,
        entry_point_type: EntryPointType,
    ) -> anyhow::Result<Vec<SierraEntryPoint>> {
        Ok(entries
            .get(&entry_point_type)
            .ok_or(anyhow!("Missing {:?} entry point", entry_point_type))?
            .iter()
            .enumerate()
            .map(|(index, e)| to_entry_point(e.clone(), index as u64))
            .collect())
    }

    let constructor = collect_entry_points(entries, EntryPointType::Constructor).unwrap_or_default();
    let external = collect_entry_points(entries, EntryPointType::External)?;
    let l1_handler = collect_entry_points(entries, EntryPointType::L1Handler).unwrap_or_default();

    Ok(EntryPointsByType { constructor, external, l1_handler })
}

/// Returns a [IndexMap<EntryPointType, Vec<EntryPoint>>] from a
/// [LegacyEntryPointsByType]
fn from_legacy_entry_points_by_type(entries: &RawLegacyEntryPoints) -> IndexMap<EntryPointType, Vec<EntryPoint>> {
    core::iter::empty()
        .chain(entries.constructor.iter().map(|entry| (EntryPointType::Constructor, entry)))
        .chain(entries.external.iter().map(|entry| (EntryPointType::External, entry)))
        .chain(entries.l1_handler.iter().map(|entry| (EntryPointType::L1Handler, entry)))
        .fold(IndexMap::new(), |mut map, (entry_type, entry)| {
            map.entry(entry_type).or_default().push(from_legacy_entry_point(entry));
            map
        })
}

/// Returns a [LegacyContractEntryPoint] (starknet-rs) from a [EntryPoint]
/// (starknet-api)
fn to_legacy_entry_point(entry_point: EntryPoint) -> LegacyContractEntryPoint {
    let selector = entry_point.selector.0.to_felt();
    let offset = entry_point.offset.0;
    LegacyContractEntryPoint { selector, offset }
}

/// Returns a [SierraEntryPoint] (starknet-rs) from a [EntryPointV1]
/// (starknet-api)
fn to_entry_point(entry_point: EntryPointV1, index: u64) -> SierraEntryPoint {
    let selector = entry_point.selector.0.to_felt();
    let function_idx = index;
    SierraEntryPoint { selector, function_idx }
}

/// Returns a [EntryPoint] (starknet-api) from a [LegacyContractEntryPoint]
/// (starknet-rs)
fn from_legacy_entry_point(entry_point: &RawLegacyEntryPoint) -> EntryPoint {
    let selector = EntryPointSelector(StarkFelt::new_unchecked(entry_point.selector.to_bytes_be()));
    let offset = EntryPointOffset(entry_point.offset.into());
    EntryPoint { selector, offset }
}

use starknet_core::types::{
    FunctionStateMutability, LegacyEventAbiType, LegacyFunctionAbiType, LegacyStructAbiType, LegacyTypedParameter,
};
use starknet_providers::sequencer::models::DeployedClass;
use starknet_types_core::felt::Felt;

// Wrapper Class conversion

impl TryFrom<serde_json::Value> for ContractClassWrapper {
    type Error = anyhow::Error;

    fn try_from(contract_class: serde_json::Value) -> Result<Self, Self::Error> {
        let contract = from_rpc_contract_class(&contract_class)?;

        let abi_value = contract_class
            .get("abi")
            .ok_or_else(|| anyhow::anyhow!("Missing `abi` field in contract class"))?;
        let abi_string = serde_json::to_string(abi_value)?;

        let sierra_program_exists = contract_class.get("sierra_program").is_some();
        let abi = if sierra_program_exists {
            ContractAbi::Sierra(abi_string.clone())
        } else {
            ContractAbi::Cairo(Some(abi_string.clone()))
        };

        let sierra_program_length = contract_class
            .get("sierra_program")
            .and_then(|sierra_program| sierra_program.as_array().map(|arr| arr.len()))
            .unwrap_or(0) as u64;

        let abi_length = abi_string.len() as u64;

        Ok(Self { contract, abi, sierra_program_length, abi_length })
    }
}


impl TryInto<ContractClassCore> for ContractClassWrapper {
    type Error = anyhow::Error;

    fn try_into(self) -> Result<ContractClassCore, Self::Error> {
        match self.contract {
            ContractClassBlockifier::V0(contract_class) => {
                match self.abi {
                    ContractAbi::Cairo(opt_string) => {
                        to_contract_class_cairo(&contract_class, opt_string)
                    }
                    _ => Err(anyhow::anyhow!("Invalid ABI type for Cairo")),
                }
            }
            ContractClassBlockifier::V1(contract_class) => {
                match self.abi {
                    ContractAbi::Sierra(string) => {
                        to_contract_class_sierra(&contract_class, string)
                    }
                    _ => Err(anyhow::anyhow!("Invalid ABI type for Sierra")),
                }
            }
        }
    }
}


fn to_rpc_contract_abi(abi: Option<Vec<AbiEntryWrapper>>) -> Option<Vec<RawLegacyAbiEntry>> {
    abi.map(|entries| entries.into_iter().map(|v| v.into()).collect())
}

fn from_rpc_contract_abi(abi: Option<Vec<RawLegacyAbiEntry>>) -> Option<Vec<AbiEntryWrapper>> {
    abi.map(|entries| entries.into_iter().map(AbiEntryWrapper::from).collect())
}

impl From<RawLegacyAbiEntry> for AbiEntryWrapper {
    fn from(abi_entry: RawLegacyAbiEntry) -> Self {
        match abi_entry {
            RawLegacyAbiEntry::Function(abi_function) => {
                AbiEntryWrapper::Function(AbiFunctionEntryWrapper::from(abi_function))
            }
            RawLegacyAbiEntry::Event(abi_event) => AbiEntryWrapper::Event(AbiEventEntryWrapper::from(abi_event)),
            RawLegacyAbiEntry::Struct(abi_struct) => AbiEntryWrapper::Struct(AbiStructEntryWrapper::from(abi_struct)),
            RawLegacyAbiEntry::Constructor(abi_constructor) => {
                AbiEntryWrapper::Function(AbiFunctionEntryWrapper::from(RawLegacyFunction {
                    name: "constructor".to_string(),
                    inputs: abi_constructor.inputs,
                    outputs: vec![],
                    state_mutability: None,
                }))
            }
            RawLegacyAbiEntry::L1Handler(abi_l1_handler) => {
                AbiEntryWrapper::Function(AbiFunctionEntryWrapper::from(RawLegacyFunction {
                    name: "l1_handler".to_string(),
                    inputs: abi_l1_handler.inputs,
                    outputs: vec![],
                    state_mutability: None,
                }))
            }
        }
    }
}

impl From<AbiEntryWrapper> for RawLegacyAbiEntry {
    fn from(abi_entry: AbiEntryWrapper) -> Self {
        match abi_entry {
            AbiEntryWrapper::Function(abi_function) => RawLegacyAbiEntry::Function(abi_function.into()),
            AbiEntryWrapper::Event(abi_event) => RawLegacyAbiEntry::Event(abi_event.into()),
            AbiEntryWrapper::Struct(abi_struct) => RawLegacyAbiEntry::Struct(abi_struct.into()),
        }
    }
}

// Function ABI Entry conversion

impl From<RawLegacyFunction> for AbiFunctionEntryWrapper {
    fn from(abi_function_entry: RawLegacyFunction) -> Self {
        Self {
            name: abi_function_entry.name,
            inputs: abi_function_entry.inputs.into_iter().map(AbiTypedParameterWrapper::from).collect(),
            outputs: abi_function_entry.outputs.into_iter().map(AbiTypedParameterWrapper::from).collect(),
            state_mutability: abi_function_entry.state_mutability.map(AbiFunctionStateMutabilityWrapper::from),
        }
    }
}

impl From<AbiFunctionEntryWrapper> for RawLegacyFunction {
    fn from(abi_function_entry: AbiFunctionEntryWrapper) -> Self {
        RawLegacyFunction {
            name: abi_function_entry.name,
            inputs: abi_function_entry.inputs.into_iter().map(|v| v.into()).collect(),
            outputs: abi_function_entry.outputs.into_iter().map(|v| v.into()).collect(),
            state_mutability: abi_function_entry.state_mutability.map(|v| v.into()),
        }
    }
}

impl From<LegacyFunctionAbiType> for AbiFunctionTypeWrapper {
    fn from(abi_func_type: LegacyFunctionAbiType) -> Self {
        match abi_func_type {
            LegacyFunctionAbiType::Function => AbiFunctionTypeWrapper::Function,
            LegacyFunctionAbiType::L1Handler => AbiFunctionTypeWrapper::L1handler,
            LegacyFunctionAbiType::Constructor => AbiFunctionTypeWrapper::Constructor,
        }
    }
}

impl From<AbiFunctionTypeWrapper> for LegacyFunctionAbiType {
    fn from(abi_function_type: AbiFunctionTypeWrapper) -> Self {
        match abi_function_type {
            AbiFunctionTypeWrapper::Function => LegacyFunctionAbiType::Function,
            AbiFunctionTypeWrapper::L1handler => LegacyFunctionAbiType::L1Handler,
            AbiFunctionTypeWrapper::Constructor => LegacyFunctionAbiType::Constructor,
        }
    }
}

impl From<FunctionStateMutability> for AbiFunctionStateMutabilityWrapper {
    fn from(abi_func_state_mutability: FunctionStateMutability) -> Self {
        match abi_func_state_mutability {
            FunctionStateMutability::View => AbiFunctionStateMutabilityWrapper::View,
        }
    }
}

impl From<AbiFunctionStateMutabilityWrapper> for FunctionStateMutability {
    fn from(abi_func_state_mutability: AbiFunctionStateMutabilityWrapper) -> Self {
        match abi_func_state_mutability {
            AbiFunctionStateMutabilityWrapper::View => FunctionStateMutability::View,
        }
    }
}

// Event ABI Entry conversion

impl From<RawLegacyEvent> for AbiEventEntryWrapper {
    fn from(abi_event_entry: RawLegacyEvent) -> Self {
        Self {
            name: abi_event_entry.name,
            keys: abi_event_entry.keys.into_iter().map(AbiTypedParameterWrapper::from).collect(),
            data: abi_event_entry.data.into_iter().map(AbiTypedParameterWrapper::from).collect(),
        }
    }
}

impl From<AbiEventEntryWrapper> for RawLegacyEvent {
    fn from(abi_event_entry: AbiEventEntryWrapper) -> Self {
        RawLegacyEvent {
            name: abi_event_entry.name,
            keys: abi_event_entry.keys.into_iter().map(|v| v.into()).collect(),
            data: abi_event_entry.data.into_iter().map(|v| v.into()).collect(),
        }
    }
}

impl From<LegacyEventAbiType> for AbiEventTypeWrapper {
    fn from(abi_entry_type: LegacyEventAbiType) -> Self {
        match abi_entry_type {
            LegacyEventAbiType::Event => AbiEventTypeWrapper::Event,
        }
    }
}

impl From<AbiEventTypeWrapper> for LegacyEventAbiType {
    fn from(abi_event_type: AbiEventTypeWrapper) -> Self {
        match abi_event_type {
            AbiEventTypeWrapper::Event => LegacyEventAbiType::Event,
        }
    }
}

// Struct ABI Entry conversion

impl From<RawLegacyStruct> for AbiStructEntryWrapper {
    fn from(abi_struct_entry: RawLegacyStruct) -> Self {
        Self {
            name: abi_struct_entry.name,
            size: abi_struct_entry.size,
            members: abi_struct_entry.members.into_iter().map(AbiStructMemberWrapper::from).collect(),
        }
    }
}

impl From<AbiStructEntryWrapper> for RawLegacyStruct {
    fn from(abi_struct_entry: AbiStructEntryWrapper) -> Self {
        RawLegacyStruct {
            name: abi_struct_entry.name,
            size: abi_struct_entry.size,
            members: abi_struct_entry.members.into_iter().map(RawLegacyMember::from).collect(),
        }
    }
}

impl From<LegacyStructAbiType> for AbiStructTypeWrapper {
    fn from(abi_struct_type: LegacyStructAbiType) -> Self {
        match abi_struct_type {
            LegacyStructAbiType::Struct => AbiStructTypeWrapper::Struct,
        }
    }
}

impl From<AbiStructTypeWrapper> for LegacyStructAbiType {
    fn from(abi_struct_type: AbiStructTypeWrapper) -> Self {
        match abi_struct_type {
            AbiStructTypeWrapper::Struct => LegacyStructAbiType::Struct,
        }
    }
}

impl From<RawLegacyMember> for AbiStructMemberWrapper {
    fn from(abi_struct_member: RawLegacyMember) -> Self {
        Self { name: abi_struct_member.name, r#type: abi_struct_member.r#type, offset: abi_struct_member.offset }
    }
}

impl From<AbiStructMemberWrapper> for RawLegacyMember {
    fn from(abi_struct_member: AbiStructMemberWrapper) -> Self {
        RawLegacyMember {
            name: abi_struct_member.name,
            r#type: abi_struct_member.r#type,
            offset: abi_struct_member.offset,
        }
    }
}

impl From<LegacyTypedParameter> for AbiTypedParameterWrapper {
    fn from(abi_typed_parameter: LegacyTypedParameter) -> Self {
        Self { name: abi_typed_parameter.name, r#type: abi_typed_parameter.r#type }
    }
}

impl From<AbiTypedParameterWrapper> for LegacyTypedParameter {
    fn from(abi_typed_parameter: AbiTypedParameterWrapper) -> Self {
        LegacyTypedParameter { name: abi_typed_parameter.name, r#type: abi_typed_parameter.r#type }
    }
}
