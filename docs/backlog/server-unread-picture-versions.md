# Collect picture versions nothing reads

Each fill of an AMS object stores its bytes under `ams/<object>/<version>` and the fill's transaction names the
version the object reads from (`crates/ngteams-server/src/media/ams.rs`, `store`). The version it replaced is
removed from the store right after, by one delete that is only logged when it fails. The row no longer names that
version and its bytes are no longer counted, so nothing removes it later. The same holds for a node that stops
between the put and the transaction, and for a fill whose transaction failed while the row couldn't be read back.

Such bytes are read by nobody and counted against no quota; they only take space in the bucket. Add a way to
remove them that can be retried: record a version when it is retired (in the fill's transaction) and clear the
record once the delete worked, or have housekeeping list `ams/<object>/` and remove every key the object's row
doesn't name and that is older than a fill can take. Cover it with a store whose delete fails once. Deferred from
task 182's third review round: it needs a table or a listing call the store trait doesn't have yet.
