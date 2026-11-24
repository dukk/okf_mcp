## Added
- `[auth]` config keys `product_name`, `support_contact` and `feedback_url`: the defaults for the three branding settings, which a value saved in Settings still overrides until Reset. A deployment can now be branded from its config file alone. The file's values are checked exactly as `okf-mcp settings set` checks them (`store::settings::check_branding_defaults`), and a bad one stops start-up with an `auth.<key>` message.
- `[auth] mcp_server_name` (config-only, default `okf_mcp`, the crate name): the MCP `serverInfo.name`. 1 to 64 ASCII letters, digits, `-`, `_` and `.`; anything else stops start-up.
- `serverInfo.title` is set to the effective `product_name` (config default or Settings override) when each MCP session starts. `OkfServer` builds its `Implementation` once per session in `new`.
- `AuthConfig::default_brand()`: the branding the config gives, with the product name trimmed as a stored name is.

## Changed
- The passkey relying-party name (shown by browsers and password managers as "Save a passkey for ...") is now `[auth] product_name`, not the hardcoded `okf`. WebAuthn fixes it at start-up, so a later Settings override does not change it.
- The console shell (`index.html`) no longer hardcodes `okf` in the tab title, login card or sidebar. `applyBrand()` fills them from `/ui/brand.json`, so a branded deployment never shows "okf" while the page loads.
- `mail::queue::enqueue` and `enqueue_in` take the config's branding defaults (`&Brand`), and so does `settings::brand_in`. A mail queued with no stored branding row now names the configured product, not `okf`.
- `UnlockPolicy` carries `brand_defaults` for the owners' notification email, and is no longer `Copy`.
- The Settings page's hint no longer claims the product name defaults to `okf`.
- `send_feedback`'s description and the MCP instructions line that offers it (`mcp::feedback_instructions`, which replaces the `FEEDBACK_INSTRUCTIONS` constant) name the effective `product_name` (config default or Settings override) when each session starts, instead of the hardcoded "Telekin". The description written on the tool, which the public tool reference (`docs/reference/tools.md`) shows, now says "this server". `mcp::with_reference` builds the reference-link suffix for both paths.

## Notes
- `okf` remains the built-in default when neither the config nor a setting names a product, so existing deployments are unchanged.
- Internal identifiers keep the `okf` name: the `X-OKF-*` headers, `okf.*` localStorage keys, metric and span names, and worker names. They are protocol and telemetry names, not branding.
