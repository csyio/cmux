//! The `cmux.protocol/2` error codes this server emits; each one is in
//! spec/resource-operations-v2.json.

pub(crate) const RESOURCE_ERROR_CODES: &[&str] = &[
    "app.screen_fixed",
    "confirmation.required",
    "creation.conflict",
    "cursor.gap",
    "cursor.invalid",
    "home.not_closable",
    "home.pinned_first",
    "idempotency.conflict",
    "local.io",
    "mutation.indeterminate",
    "operation.failed",
    "operation.unsupported",
    "resource.not_found",
    "revision.conflict",
    "selector.ambiguous",
    "selector.invalid",
    "selector.not_found",
    "selector.wrong_parent",
    "terminal.closed",
    "transport.closed",
    "validation.invalid",
];

pub(crate) fn is_catalog_error_code(code: &str) -> bool {
    RESOURCE_ERROR_CODES.contains(&code)
}
