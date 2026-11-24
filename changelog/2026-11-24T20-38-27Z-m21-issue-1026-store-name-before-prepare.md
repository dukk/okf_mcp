# M21 issue 1026: a refused store add no longer claims the target database

Brooke's re-review of the milestone PR found that `stores::add` migrated the
target database, writing its identity row, before the insert refused a taken
name or a taken host, port and database. The database then belonged to a store
that was never registered, and a retry under a corrected name was refused.

## What changed

- `add` runs on one account transaction (one connection; a pool of one is
  enough, and concurrent adds never wait on a second connection each). It
  takes two advisory locks (the name; the host, port and database), refuses a
  name or location that already has a row, then claims the target database,
  then inserts the row and audit entry in the same transaction.
- `Stores::check` and `Stores::prepare` are replaced by `Stores::claim`, which
  opens the target database and takes an advisory lock inside it
  (`db::STORE_ADD_LOCK_KEY`), held until after the registry commit
  (`StoreClaim::release`). On one connection under that lock it refuses the
  account database and an identity naming a registered store, replaces an
  identity naming a store with no row (an add that failed after migrating, or
  died) through `db::replace_store_identity`, and migrates. The target lock
  orders adds of one physical database under any host spelling or name, which
  closes the alias window a name and location lock alone left (review of
  PR 1027). A retry, or an add under another name, succeeds; an alias of a
  registered store is still refused.
- Why not write the identity in the insert's transaction: the migration must
  run before the row exists (the row makes the store reachable to every
  replica), and it runs on another database, so it cannot share a transaction.

## Tests

`a_taken_name_is_refused_before_the_database_is_claimed`,
`a_taken_location_is_refused_before_the_database_is_claimed`,
`a_database_claimed_by_an_unregistered_store_can_be_added_again`,
`concurrent_adds_of_one_name_leave_the_loser_database_unclaimed`, all through
public `add`; Brooke's review added
`a_store_can_be_added_on_a_one_connection_account_pool`,
`concurrent_adds_on_a_bounded_account_pool_all_succeed`,
`an_add_waits_while_another_holds_the_target_database` and
`concurrent_alias_adds_of_one_database_register_exactly_one` (a barrier, five
rounds; the interleaving itself is up to the scheduler, the lock test above is
the deterministic one).
