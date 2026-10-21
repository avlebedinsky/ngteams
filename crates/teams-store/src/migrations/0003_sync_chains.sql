-- A conversation's sync keeps two chains of older pages apart, and how far back its history read reached.

-- the rest of a sync answer cut at the page cap: read before older history, never in its place
ALTER TABLE conversation_sync ADD COLUMN gap_link TEXT;

-- the oldest arrival time (ms) on the page `backward_link` came with: everything newer is stored; none when unknown
ALTER TABLE conversation_sync ADD COLUMN backward_oldest INTEGER;
