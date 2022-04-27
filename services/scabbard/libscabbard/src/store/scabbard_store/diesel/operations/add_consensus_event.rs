// Copyright 2018-2022 Cargill Incorporated
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::convert::TryFrom;

#[cfg(feature = "postgres")]
use diesel::pg::PgConnection;
#[cfg(feature = "sqlite")]
use diesel::sqlite::SqliteConnection;
use diesel::{dsl::insert_into, prelude::*};
use splinter::error::{InternalError, InvalidStateError};
use splinter::service::FullyQualifiedServiceId;

use crate::store::scabbard_store::diesel::{
    models::{
        Consensus2pcCoordinatorContextModel, Consensus2pcParticipantContextModel,
        InsertableTwoPcConsensusEventModel, TwoPcConsensusDeliverEventModel,
        TwoPcConsensusStartEventModel, TwoPcConsensusVoteEventModel,
    },
    schema::{
        consensus_2pc_coordinator_context, consensus_2pc_participant_context,
        two_pc_consensus_deliver_event, two_pc_consensus_event, two_pc_consensus_start_event,
        two_pc_consensus_vote_event,
    },
};
use crate::store::scabbard_store::ScabbardStoreError;
use crate::store::scabbard_store::{
    event::ScabbardConsensusEvent,
    two_phase::{event::Scabbard2pcEvent, message::Scabbard2pcMessage},
};

use super::ScabbardStoreOperations;

pub(in crate::store::scabbard_store::diesel) trait AddEventOperation {
    fn add_consensus_event(
        &self,
        service_id: &FullyQualifiedServiceId,
        epoch: u64,
        event: ScabbardConsensusEvent,
    ) -> Result<i64, ScabbardStoreError>;
}

#[cfg(feature = "sqlite")]
impl<'a> AddEventOperation for ScabbardStoreOperations<'a, SqliteConnection> {
    fn add_consensus_event(
        &self,
        service_id: &FullyQualifiedServiceId,
        epoch: u64,
        event: ScabbardConsensusEvent,
    ) -> Result<i64, ScabbardStoreError> {
        self.conn.transaction::<_, _, _>(|| {
            let ScabbardConsensusEvent::Scabbard2pcConsensusEvent(event) = event;
            let epoch = i64::try_from(epoch).map_err(|err| {
                ScabbardStoreError::Internal(InternalError::from_source(Box::new(err)))
            })?;
            // check to see if a coordinator context with the given epoch and service_id exists
            let coordinator_context = consensus_2pc_coordinator_context::table
                .filter(consensus_2pc_coordinator_context::epoch.eq(epoch).and(
                    consensus_2pc_coordinator_context::service_id.eq(format!("{}", service_id)),
                ))
                .first::<Consensus2pcCoordinatorContextModel>(self.conn)
                .optional()?;

            // check to see if a participant context with the given epoch and service_id exists
            let participant_context = consensus_2pc_participant_context::table
                .filter(consensus_2pc_participant_context::epoch.eq(epoch).and(
                    consensus_2pc_participant_context::service_id.eq(format!("{}", service_id)),
                ))
                .first::<Consensus2pcParticipantContextModel>(self.conn)
                .optional()?;

            let position = two_pc_consensus_event::table
                .filter(
                    two_pc_consensus_event::service_id
                        .eq(format!("{}", service_id))
                        .and(two_pc_consensus_event::epoch.eq(epoch)),
                )
                .order(two_pc_consensus_event::position.desc())
                .select(two_pc_consensus_event::position)
                .first::<i32>(self.conn)
                .optional()?
                .unwrap_or(0)
                + 1;

            if coordinator_context.is_some() {
                // return an error if there is both a coordinator and a participant context for the
                // given service_id and epoch
                if participant_context.is_some() {
                    return Err(ScabbardStoreError::InvalidState(
                        InvalidStateError::with_message(format!(
                            "Failed to add consensus event, contexts found for participant and 
                            coordinator with service_id: {} epoch: {} ",
                            service_id, epoch
                        )),
                    ));
                }

                let insertable_event = InsertableTwoPcConsensusEventModel {
                    service_id: format!("{}", service_id),
                    epoch,
                    executed_at: None,
                    position,
                    event_type: String::from(&event),
                };

                insert_into(two_pc_consensus_event::table)
                    .values(vec![insertable_event])
                    .execute(self.conn)?;
                let event_id = two_pc_consensus_event::table
                    .order(two_pc_consensus_event::id.desc())
                    .select(two_pc_consensus_event::id)
                    .first::<i64>(self.conn)?;

                match event {
                    Scabbard2pcEvent::Alarm() => Ok(event_id),
                    Scabbard2pcEvent::Deliver(receiving_process, message) => {
                        let (message_type, vote_response) = match message {
                            Scabbard2pcMessage::DecisionRequest(_) => {
                                (String::from(&message), None)
                            }
                            Scabbard2pcMessage::VoteResponse(_, true) => {
                                (String::from(&message), Some("TRUE".to_string()))
                            }
                            Scabbard2pcMessage::VoteResponse(_, false) => {
                                (String::from(&message), Some("FALSE".to_string()))
                            }
                            _ => {
                                return Err(ScabbardStoreError::InvalidState(
                                    InvalidStateError::with_message(format!(
                                        "Failed to add consensus deliver event, invalid coordinator 
                                        message type {}",
                                        String::from(&message)
                                    )),
                                ))
                            }
                        };

                        let deliver_event = TwoPcConsensusDeliverEventModel {
                            event_id,
                            service_id: format!("{}", service_id),
                            epoch,
                            receiver_service_id: format!("{}", receiving_process),
                            message_type,
                            vote_response,
                            vote_request: None,
                        };
                        insert_into(two_pc_consensus_deliver_event::table)
                            .values(vec![deliver_event])
                            .execute(self.conn)?;
                        Ok(event_id)
                    }
                    Scabbard2pcEvent::Start(value) => {
                        let start_event = TwoPcConsensusStartEventModel {
                            event_id,
                            service_id: format!("{}", service_id),
                            epoch,
                            value,
                        };
                        insert_into(two_pc_consensus_start_event::table)
                            .values(vec![start_event])
                            .execute(self.conn)?;
                        Ok(event_id)
                    }
                    Scabbard2pcEvent::Vote(vote) => {
                        let vote = match vote {
                            true => String::from("TRUE"),
                            false => String::from("FALSE"),
                        };
                        let vote_event = TwoPcConsensusVoteEventModel {
                            event_id,
                            service_id: format!("{}", service_id),
                            epoch,
                            vote,
                        };
                        insert_into(two_pc_consensus_vote_event::table)
                            .values(vec![vote_event])
                            .execute(self.conn)?;
                        Ok(event_id)
                    }
                }
            } else if participant_context.is_some() {
                let insertable_event = InsertableTwoPcConsensusEventModel {
                    service_id: format!("{}", service_id),
                    epoch,
                    executed_at: None,
                    position,
                    event_type: String::from(&event),
                };

                insert_into(two_pc_consensus_event::table)
                    .values(vec![insertable_event])
                    .execute(self.conn)?;
                let event_id = two_pc_consensus_event::table
                    .order(two_pc_consensus_event::id.desc())
                    .select(two_pc_consensus_event::id)
                    .first::<i64>(self.conn)?;

                match event {
                    Scabbard2pcEvent::Alarm() => Ok(event_id),
                    Scabbard2pcEvent::Deliver(receiving_process, message) => {
                        let (message_type, vote_request) = match &message {
                            Scabbard2pcMessage::DecisionRequest(_) => {
                                (String::from(&message), None)
                            }
                            Scabbard2pcMessage::Commit(_) => (String::from(&message), None),
                            Scabbard2pcMessage::Abort(_) => (String::from(&message), None),
                            Scabbard2pcMessage::VoteRequest(_, value) => {
                                (String::from(&message), Some(value.clone()))
                            }
                            _ => {
                                return Err(ScabbardStoreError::InvalidState(
                                    InvalidStateError::with_message(format!(
                                        "Failed to add consensus deliver event, invalid participant 
                                        message type {}",
                                        String::from(&message)
                                    )),
                                ))
                            }
                        };

                        let deliver_event = TwoPcConsensusDeliverEventModel {
                            event_id,
                            service_id: format!("{}", service_id),
                            epoch,
                            receiver_service_id: format!("{}", receiving_process),
                            message_type,
                            vote_response: None,
                            vote_request,
                        };
                        insert_into(two_pc_consensus_deliver_event::table)
                            .values(vec![deliver_event])
                            .execute(self.conn)?;
                        Ok(event_id)
                    }
                    Scabbard2pcEvent::Vote(vote) => {
                        let vote = match vote {
                            true => String::from("TRUE"),
                            false => String::from("FALSE"),
                        };
                        let vote_event = TwoPcConsensusVoteEventModel {
                            event_id,
                            service_id: format!("{}", service_id),
                            epoch,
                            vote,
                        };
                        insert_into(two_pc_consensus_vote_event::table)
                            .values(vec![vote_event])
                            .execute(self.conn)?;
                        Ok(event_id)
                    }
                    _ => {
                        return Err(ScabbardStoreError::InvalidState(
                            InvalidStateError::with_message(format!(
                                "Failed to add consensus event, invalid participant event 
                            type {}",
                                String::from(&event)
                            )),
                        ))
                    }
                }
            } else {
                Err(ScabbardStoreError::InvalidState(
                    InvalidStateError::with_message(format!(
                        "Failed to add consensus event, a context with service_id: {} and epoch: {} 
                        does not exist",
                        service_id, epoch
                    )),
                ))
            }
        })
    }
}

#[cfg(feature = "postgres")]
impl<'a> AddEventOperation for ScabbardStoreOperations<'a, PgConnection> {
    fn add_consensus_event(
        &self,
        service_id: &FullyQualifiedServiceId,
        epoch: u64,
        event: ScabbardConsensusEvent,
    ) -> Result<i64, ScabbardStoreError> {
        self.conn.transaction::<_, _, _>(|| {
            let ScabbardConsensusEvent::Scabbard2pcConsensusEvent(event) = event;
            let epoch = i64::try_from(epoch).map_err(|err| {
                ScabbardStoreError::Internal(InternalError::from_source(Box::new(err)))
            })?;
            // check to see if a coordinator context with the given epoch and service_id exists
            let coordinator_context = consensus_2pc_coordinator_context::table
                .filter(consensus_2pc_coordinator_context::epoch.eq(epoch).and(
                    consensus_2pc_coordinator_context::service_id.eq(format!("{}", service_id)),
                ))
                .first::<Consensus2pcCoordinatorContextModel>(self.conn)
                .optional()?;

            // check to see if a participant context with the given epoch and service_id exists
            let participant_context = consensus_2pc_participant_context::table
                .filter(consensus_2pc_participant_context::epoch.eq(epoch).and(
                    consensus_2pc_participant_context::service_id.eq(format!("{}", service_id)),
                ))
                .first::<Consensus2pcParticipantContextModel>(self.conn)
                .optional()?;

            let position = two_pc_consensus_event::table
                .filter(
                    two_pc_consensus_event::service_id
                        .eq(format!("{}", service_id))
                        .and(two_pc_consensus_event::epoch.eq(epoch)),
                )
                .order(two_pc_consensus_event::position.desc())
                .select(two_pc_consensus_event::position)
                .first::<i32>(self.conn)
                .optional()?
                .unwrap_or(0)
                + 1;

            if coordinator_context.is_some() {
                // return an error if there is both a coordinator and a participant context for the
                // given service_id and epoch
                if participant_context.is_some() {
                    return Err(ScabbardStoreError::InvalidState(
                        InvalidStateError::with_message(format!(
                            "Failed to add consensus event, contexts found for participant and 
                            coordinator with service_id: {} epoch: {} ",
                            service_id, epoch
                        )),
                    ));
                }

                let insertable_event = InsertableTwoPcConsensusEventModel {
                    service_id: format!("{}", service_id),
                    epoch,
                    executed_at: None,
                    position,
                    event_type: String::from(&event),
                };

                let event_id: i64 = insert_into(two_pc_consensus_event::table)
                    .values(vec![insertable_event])
                    .returning(two_pc_consensus_event::id)
                    .get_result(self.conn)?;

                match event {
                    Scabbard2pcEvent::Alarm() => Ok(event_id),
                    Scabbard2pcEvent::Deliver(receiving_process, message) => {
                        let (message_type, vote_response) = match message {
                            Scabbard2pcMessage::DecisionRequest(_) => {
                                (String::from(&message), None)
                            }
                            Scabbard2pcMessage::VoteResponse(_, true) => {
                                (String::from(&message), Some("TRUE".to_string()))
                            }
                            Scabbard2pcMessage::VoteResponse(_, false) => {
                                (String::from(&message), Some("FALSE".to_string()))
                            }
                            _ => {
                                return Err(ScabbardStoreError::InvalidState(
                                    InvalidStateError::with_message(format!(
                                        "Failed to add consensus deliver event, invalid 
                                            coordinator message type {}",
                                        String::from(&message)
                                    )),
                                ))
                            }
                        };

                        let deliver_event = TwoPcConsensusDeliverEventModel {
                            event_id,
                            service_id: format!("{}", service_id),
                            epoch,
                            receiver_service_id: format!("{}", receiving_process),
                            message_type,
                            vote_response,
                            vote_request: None,
                        };
                        insert_into(two_pc_consensus_deliver_event::table)
                            .values(vec![deliver_event])
                            .execute(self.conn)?;
                        Ok(event_id)
                    }
                    Scabbard2pcEvent::Start(value) => {
                        let start_event = TwoPcConsensusStartEventModel {
                            event_id,
                            service_id: format!("{}", service_id),
                            epoch,
                            value,
                        };
                        insert_into(two_pc_consensus_start_event::table)
                            .values(vec![start_event])
                            .execute(self.conn)?;
                        Ok(event_id)
                    }
                    Scabbard2pcEvent::Vote(vote) => {
                        let vote = match vote {
                            true => String::from("TRUE"),
                            false => String::from("FALSE"),
                        };
                        let vote_event = TwoPcConsensusVoteEventModel {
                            event_id,
                            service_id: format!("{}", service_id),
                            epoch,
                            vote,
                        };
                        insert_into(two_pc_consensus_vote_event::table)
                            .values(vec![vote_event])
                            .execute(self.conn)?;
                        Ok(event_id)
                    }
                }
            } else if participant_context.is_some() {
                let insertable_event = InsertableTwoPcConsensusEventModel {
                    service_id: format!("{}", service_id),
                    epoch,
                    executed_at: None,
                    position,
                    event_type: String::from(&event),
                };

                let event_id: i64 = insert_into(two_pc_consensus_event::table)
                    .values(vec![insertable_event])
                    .returning(two_pc_consensus_event::id)
                    .get_result(self.conn)?;

                match event {
                    Scabbard2pcEvent::Alarm() => Ok(event_id),
                    Scabbard2pcEvent::Deliver(receiving_process, message) => {
                        let (message_type, vote_request) = match &message {
                            Scabbard2pcMessage::DecisionRequest(_) => {
                                (String::from(&message), None)
                            }
                            Scabbard2pcMessage::Commit(_) => (String::from(&message), None),
                            Scabbard2pcMessage::Abort(_) => (String::from(&message), None),
                            Scabbard2pcMessage::VoteRequest(_, value) => {
                                (String::from(&message), Some(value.clone()))
                            }
                            _ => {
                                return Err(ScabbardStoreError::InvalidState(
                                    InvalidStateError::with_message(format!(
                                        "Failed to add consensus deliver event, invalid 
                                            participant message type {}",
                                        String::from(&message)
                                    )),
                                ))
                            }
                        };

                        let deliver_event = TwoPcConsensusDeliverEventModel {
                            event_id,
                            service_id: format!("{}", service_id),
                            epoch,
                            receiver_service_id: format!("{}", receiving_process),
                            message_type,
                            vote_response: None,
                            vote_request,
                        };
                        insert_into(two_pc_consensus_deliver_event::table)
                            .values(vec![deliver_event])
                            .execute(self.conn)?;
                        Ok(event_id)
                    }
                    Scabbard2pcEvent::Vote(vote) => {
                        let vote = match vote {
                            true => String::from("TRUE"),
                            false => String::from("FALSE"),
                        };
                        let vote_event = TwoPcConsensusVoteEventModel {
                            event_id,
                            service_id: format!("{}", service_id),
                            epoch,
                            vote,
                        };
                        insert_into(two_pc_consensus_vote_event::table)
                            .values(vec![vote_event])
                            .execute(self.conn)?;
                        Ok(event_id)
                    }
                    _ => {
                        return Err(ScabbardStoreError::InvalidState(
                            InvalidStateError::with_message(format!(
                                "Failed to add consensus event, invalid participant event 
                                type {}",
                                String::from(&event)
                            )),
                        ))
                    }
                }
            } else {
                Err(ScabbardStoreError::InvalidState(
                    InvalidStateError::with_message(format!(
                        "Failed to add consensus event, a context with service_id: {} and epoch: {} 
                        does not exist",
                        service_id, epoch
                    )),
                ))
            }
        })
    }
}
