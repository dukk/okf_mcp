# M21 issue 1026: a refused store add no longer claims the target database

Brooke's re-review of the milestone PR found that `stores::add` migrated the
target database, writing its identity row, before the insert refused a taken
name or a taken host, port and database. The database then belonged to a store
that was never registered, and a retry under a corrected name was refused.

## What changed

- `add` first opens a transaction on the account database, takes two advisory
  locks (the name; the host, port and database) and refuses a name or location
  that already has a row. Only then does it check, prepare and insert. The
  transaction writes nothing and lasts to the end of the add, so adds of one
  name or one database run one at a time. It holds one account connection
  while the store's pool connects.
- `Stores::check` refuses a database whose identity names another store only
  when that store has a row. `Stores::prepare` replaces an identity naming a
  store with no row (an add that failed after migrating, or died) with the new
  store's name, through `db::replace_store_identity`. The locks make sure no
  other add is mid-way on that database. A retry, or an add under another
  name, now succeeds; an alias of a registered store is still refused.
- Why not write the identity in the insert's transaction: the migration must
  run before the row exists (the row makes the store reachable to every
  replica), and it runs on another database, so it cannot share a transaction.

## Tests

`a_taken_name_is_refused_before_the_database_is_claimed`,
`a_taken_location_is_refused_before_the_database_is_claimed`,
`a_database_claimed_by_an_unregistered_store_can_be_added_again`,
`concurrent_adds_of_one_name_leave_the_loser_database_unclaimed`, all through
public `add`.
