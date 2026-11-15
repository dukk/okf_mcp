# Fix #448: CIMD documents may list up to 32 redirect URIs

## Problem

MCPJam, an MCP inspector, could not connect: the authorize page said "This
client is not known to the server." Its `client_id` is a Client ID Metadata
Document (`https://www.mcpjam.com/.well-known/oauth/client-metadata.json`)
listing 18 redirect URIs, one per environment it runs in. Client metadata
was validated against a single cap of 10 redirect URIs, shared by dynamic
client registration and CIMD documents, so the document was refused.

## Change

- `src/oauth/clients.rs`: `validate_metadata` takes the redirect URI cap as
  a parameter. `/oauth/register` passes `MAX_REDIRECT_URIS` (10, unchanged);
  `validate_cimd_document` passes the new `MAX_CIMD_REDIRECT_URIS` (32).
- Test `a_cimd_document_may_list_more_redirect_uris_than_a_registration`:
  MCPJam's document, verbatim, validates as a CIMD document, its loopback
  redirect URI matches, and the same list is still refused as a
  registration; a document over 32 is refused.
- `docs/operations.md`: states the CIMD limit.

## Why two caps

A registration can come from anyone and is stored, so its cap stays tight.
A CIMD document is fetched from the client's own URL, is already bounded by
the 16 KiB fetch limit (`CIMD_MAX_BYTES`), and real clients publish one
document for all their environments.
