-- Indexes for two reads that went through every row of their table.

-- A rescan clears each changed file's claims before it writes them again,
-- as identifier_claim_item lets it do for identifiers.
CREATE INDEX metadata_claim_item ON metadata_claim (source_item_id, field);

-- An import of loves reads, for each item an account loved, the newest of
-- that account's love rows for it.
CREATE INDEX listener_rating_reported ON listener_rating (endpoint_id, kind, source_item_id);
