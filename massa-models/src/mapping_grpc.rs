// Copyright (c) 2023 MASSA LABS <info@massa.net>

use std::str::FromStr;

use crate::address::Address;
use crate::block::{Block, FilledBlock, SecureShareBlock};
use crate::block_header::{BlockHeader, SecuredHeader};
use crate::denunciation::DenunciationIndex;
use crate::endorsement::{Endorsement, SecureShareEndorsement};
use crate::error::ModelsError;
use crate::execution::EventFilter;
use crate::operation::{Operation, OperationId, OperationType, SecureShareOperation};
use crate::output_event::{EventExecutionContext, SCOutputEvent};
use crate::slot::{IndexedSlot, Slot};
use massa_proto::massa::api::v1 as grpc;
use massa_signature::{PublicKey, Signature};

impl From<Block> for grpc::Block {
    fn from(value: Block) -> Self {
        grpc::Block {
            header: Some(value.header.into()),
            operations: value
                .operations
                .into_iter()
                .map(|operation| operation.to_string())
                .collect(),
        }
    }
}

impl From<BlockHeader> for grpc::BlockHeader {
    fn from(value: BlockHeader) -> Self {
        let res = value.endorsements.into_iter().map(|e| e.into()).collect();

        grpc::BlockHeader {
            slot: Some(value.slot.into()),
            parents: value
                .parents
                .into_iter()
                .map(|parent| parent.to_string())
                .collect(),
            operation_merkle_root: value.operation_merkle_root.to_string(),
            endorsements: res,
        }
    }
}

impl From<FilledBlock> for grpc::FilledBlock {
    fn from(value: FilledBlock) -> Self {
        grpc::FilledBlock {
            header: Some(value.header.into()),
            operations: value
                .operations
                .into_iter()
                .map(|tuple| grpc::FilledOperationTuple {
                    operation_id: tuple.0.to_string(),
                    operation: tuple.1.map(|op| op.into()),
                })
                .collect(),
        }
    }
}

impl From<SecureShareBlock> for grpc::SignedBlock {
    fn from(value: SecureShareBlock) -> Self {
        grpc::SignedBlock {
            content: Some(value.content.into()),
            signature: value.signature.to_bs58_check(),
            content_creator_pub_key: value.content_creator_pub_key.to_string(),
            content_creator_address: value.content_creator_address.to_string(),
            id: value.id.to_string(),
        }
    }
}

impl From<SecuredHeader> for grpc::SignedBlockHeader {
    fn from(value: SecuredHeader) -> Self {
        grpc::SignedBlockHeader {
            content: Some(value.content.into()),
            signature: value.signature.to_bs58_check(),
            content_creator_pub_key: value.content_creator_pub_key.to_string(),
            content_creator_address: value.content_creator_address.to_string(),
            id: value.id.to_string(),
        }
    }
}

impl From<Endorsement> for grpc::Endorsement {
    fn from(value: Endorsement) -> Self {
        grpc::Endorsement {
            slot: Some(value.slot.into()),
            index: value.index,
            endorsed_block: value.endorsed_block.to_string(),
        }
    }
}

impl From<SecureShareEndorsement> for grpc::SignedEndorsement {
    fn from(value: SecureShareEndorsement) -> Self {
        grpc::SignedEndorsement {
            content: Some(value.content.into()),
            signature: value.signature.to_bs58_check(),
            content_creator_pub_key: value.content_creator_pub_key.to_string(),
            content_creator_address: value.content_creator_address.to_string(),
            id: value.id.to_string(),
        }
    }
}

impl From<OperationType> for grpc::OperationType {
    fn from(operation_type: OperationType) -> grpc::OperationType {
        let mut grpc_operation_type = grpc::OperationType::default();
        match operation_type {
            OperationType::Transaction {
                recipient_address,
                amount,
            } => {
                let transaction = grpc::Transaction {
                    recipient_address: recipient_address.to_string(),
                    amount: amount.to_raw(),
                };
                grpc_operation_type.transaction = Some(transaction);
            }
            OperationType::RollBuy { roll_count } => {
                let roll_buy = grpc::RollBuy { roll_count };
                grpc_operation_type.roll_buy = Some(roll_buy);
            }
            OperationType::RollSell { roll_count } => {
                let roll_sell = grpc::RollSell { roll_count };
                grpc_operation_type.roll_sell = Some(roll_sell);
            }
            OperationType::ExecuteSC {
                data,
                max_gas,
                max_coins,
                datastore,
            } => {
                let execute_sc = grpc::ExecuteSc {
                    data,
                    max_coins: max_coins.to_raw(),
                    max_gas,
                    datastore: datastore
                        .into_iter()
                        .map(|(key, value)| grpc::BytesMapFieldEntry { key, value })
                        .collect(),
                };
                grpc_operation_type.execut_sc = Some(execute_sc);
            }
            OperationType::CallSC {
                target_addr,
                target_func,
                param,
                max_gas,
                coins,
            } => {
                let call_sc = grpc::CallSc {
                    target_addr: target_addr.to_string(),
                    target_func,
                    param,
                    max_gas,
                    coins: coins.to_raw(),
                };
                grpc_operation_type.call_sc = Some(call_sc);
            }
        }

        grpc_operation_type
    }
}

impl From<Operation> for grpc::Operation {
    fn from(op: Operation) -> Self {
        grpc::Operation {
            fee: op.fee.to_raw(),
            expire_period: op.expire_period,
            op: Some(op.op.into()),
        }
    }
}

impl From<OperationType> for grpc::OpType {
    fn from(value: OperationType) -> Self {
        match value {
            OperationType::Transaction { .. } => grpc::OpType::Transaction,
            OperationType::RollBuy { .. } => grpc::OpType::RollBuy,
            OperationType::RollSell { .. } => grpc::OpType::RollSell,
            OperationType::ExecuteSC { .. } => grpc::OpType::ExecuteSc,
            OperationType::CallSC { .. } => grpc::OpType::CallSc,
        }
    }
}

impl From<SecureShareOperation> for grpc::SignedOperation {
    fn from(value: SecureShareOperation) -> Self {
        grpc::SignedOperation {
            content: Some(value.content.into()),
            signature: value.signature.to_bs58_check(),
            content_creator_pub_key: value.content_creator_pub_key.to_string(),
            content_creator_address: value.content_creator_address.to_string(),
            id: value.id.to_string(),
        }
    }
}

impl From<IndexedSlot> for grpc::IndexedSlot {
    fn from(s: IndexedSlot) -> Self {
        grpc::IndexedSlot {
            index: s.index as u64,
            slot: Some(s.slot.into()),
        }
    }
}

impl From<Slot> for grpc::Slot {
    fn from(s: Slot) -> Self {
        grpc::Slot {
            period: s.period,
            thread: s.thread as u32,
        }
    }
}

impl From<grpc::Slot> for Slot {
    fn from(s: grpc::Slot) -> Self {
        Slot {
            period: s.period,
            thread: s.thread as u8,
        }
    }
}

impl TryFrom<grpc::GetScExecutionEventsFilter> for EventFilter {
    type Error = crate::error::ModelsError;

    fn try_from(filter: grpc::GetScExecutionEventsFilter) -> Result<Self, Self::Error> {
        let status_final = grpc::ScExecutionEventStatus::Final as i32;
        let status_error = grpc::ScExecutionEventStatus::Failure as i32;
        Ok(Self {
            start: filter.start_slot.map(|slot| slot.into()),
            end: filter.end_slot.map(|slot| slot.into()),
            emitter_address: filter
                .emitter_address
                .map(|address| Address::from_str(&address))
                .transpose()?,
            original_caller_address: filter
                .caller_address
                .map(|address| Address::from_str(&address))
                .transpose()?,
            original_operation_id: filter
                .original_operation_id
                .map(|operation_id| OperationId::from_str(&operation_id))
                .transpose()?,
            is_final: Some(filter.status.contains(&status_final)),
            is_error: Some(filter.status.contains(&status_error)),
        })
    }
}

impl From<SCOutputEvent> for grpc::ScExecutionEvent {
    fn from(value: SCOutputEvent) -> Self {
        grpc::ScExecutionEvent {
            context: Some(value.context.into()),
            data: value.data,
        }
    }
}

impl From<EventExecutionContext> for grpc::ScExecutionEventContext {
    fn from(value: EventExecutionContext) -> Self {
        let id_str = format!(
            "{}{}{}",
            &value.slot.period, &value.slot.thread, &value.index_in_slot
        );
        let id = bs58::encode(id_str.as_bytes()).with_check().into_string();
        Self {
            id,
            origin_slot: Some(value.slot.into()),
            block_id: value.block.map(|id| id.to_string()),
            index_in_slot: value.index_in_slot,
            call_stack: value
                .call_stack
                .into_iter()
                .map(|a| a.to_string())
                .collect(),
            origin_operation_id: value.origin_operation_id.map(|id| id.to_string()),
            status: {
                let mut status = Vec::new();
                if value.read_only {
                    status.push(grpc::ScExecutionEventStatus::ReadOnly as i32);
                }
                if value.is_error {
                    status.push(grpc::ScExecutionEventStatus::Failure as i32);
                }
                if value.is_final {
                    status.push(grpc::ScExecutionEventStatus::Final as i32);
                }

                status
            },
        }
    }
}

impl From<DenunciationIndex> for grpc::DenunciationIndex {
    fn from(value: DenunciationIndex) -> Self {
        grpc::DenunciationIndex {
            entry: Some(match value {
                DenunciationIndex::BlockHeader { slot } => {
                    grpc::denunciation_index::Entry::BlockHeader(grpc::DenunciationBlockHeader {
                        slot: Some(slot.into()),
                    })
                }
                DenunciationIndex::Endorsement { slot, index } => {
                    grpc::denunciation_index::Entry::Endorsement(grpc::DenunciationEndorsement {
                        slot: Some(slot.into()),
                        index,
                    })
                }
            }),
        }
    }
}

/// Converts a gRPC `SecureShare` into a byte vector
pub fn secure_share_to_vec(value: grpc::SecureShare) -> Result<Vec<u8>, ModelsError> {
    let pub_key = PublicKey::from_str(&value.content_creator_pub_key)?;
    let pub_key_b = pub_key.to_bytes();
    // Concatenate signature, public key, and data into a single byte vector
    let mut serialized_content =
        Vec::with_capacity(value.signature.len() + pub_key_b.len() + value.serialized_data.len());
    serialized_content
        .extend_from_slice(&Signature::from_str(&value.signature).map(|value| value.to_bytes())?);
    serialized_content.extend_from_slice(&pub_key_b);
    serialized_content.extend_from_slice(&value.serialized_data);

    Ok(serialized_content)
}
