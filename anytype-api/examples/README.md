# anytype Examples

Set environment flags

```sh
# headless cli uses port 31012. desktop port 31009
export ANYTYPE_URL=http://127.0.0.1:31012
# path to file containing api key
export ANYTYPE_KEY_FILE=$HOME/.config/anytype/api.key
# set space id for testing. If not set, uses first space with "test" in the name
export ANYTYPE_SPACE_ID=
# optional: enable debug logging
export RUST_LOG=
# optional: disable rate limits. If not disabled, tests will take longer to run
export ANYTYPE_DISABLE_RATE_LIMIT=1
```

Run an example with:

```sh
cargo run --example list_spaces
```

## Examples

### Basic

- `interactive_auth` - interactive authentication flow and key storage.
- `list_spaces` - list all spaces.
- `list_types_and_properties` - list types and properties in a space.
- `list_tasks` - list tasks.
- `create_object` - create a page with markdown and properties.
- `update_object_properties` - update object properties on an existing object.
- `update_markdown_body` - update an object's markdown body.

### Intermediate

- `filters_basic` - list objects using simple filters.
- `filter_expressions` - build OR filter expressions for search.
- `search_global` - global search across spaces.
- `search_in_space` - search in a space with filters and sorting.
- `templates` - list templates for a type.

### Advanced

- `retry_eventual_consistency` - enable read-after-write verification.
- `pagination_stream` - collect all pages or stream results.
- `views_list_objects` - list views and objects in a collection/query.
