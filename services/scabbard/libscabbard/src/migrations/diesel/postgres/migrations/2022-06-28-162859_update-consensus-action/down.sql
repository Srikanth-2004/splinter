-- Copyright 2018-2022 Cargill Incorporated
--
-- Licensed under the Apache License, Version 2.0 (the "License");
-- you may not use this file except in compliance with the License.
-- You may obtain a copy of the License at
--
--     http://www.apache.org/licenses/LICENSE-2.0
--
-- Unless required by applicable law or agreed to in writing, software
-- distributed under the License is distributed on an "AS IS" BASIS,
-- WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
-- See the License for the specific language governing permissions and
-- limitations under the License.
-- -----------------------------------------------------------------------------

ALTER TABLE consensus_2pc_action DROP COLUMN action_type;

DROP TYPE action_type;

ALTER TABLE consensus_2pc_action DROP COLUMN event_id;

ALTER TABLE consensus_2pc_action 
ALTER COLUMN executed_at type BIGINT 
USING CAST(EXTRACT(epoch from executed_at) as BIGINT);
