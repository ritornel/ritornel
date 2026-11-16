//! Pure logic: resolver, history derivation, play rule and planner. No I/O
//! and no clock.

// Nothing here uses the contract yet. The import keeps the dependency in
// place, and Cargo's unused-dependency lint quiet, until something does.
use ritornel_contract as _;
