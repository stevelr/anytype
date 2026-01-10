# anytype-cli

Command-line interface for the Anytype local API, built on [`anytype`](https://github.com/stevelr/anytype).

## Install / Run

````sh
cargo install --path .
```


```sh
anyr \
    --url "http://127.0.0.1:31009" \
    --keyfile-path "$HOME/.config/anytype/api.key" \
    --help
```

## Examples

```sh
# Auth
anyr auth login
anyr auth status

# List spaces
anyr space list -t

# get space id for space named "Work"
# filter on server and take first result, or filter from results
anyr space list --filter name=Work --json  | jq -r '.items[0].id]'
anyr space list --json | jq -r '.items[] | select(.name == "Work") | .id'

# List objects in a space
anyr object list <SPACE_ID> -t

# List collections
anyr object list --type collection <SPACE_ID> -t
```

### List items in a collection

Example: list all my planned trips

```sh
space_name="Personal"
collection_name="Trips"

# get space_id
personal_space=$(anyr space list --filter name="$space_name" --json  | jq -r '.items[0].id]')
# get collection id in space
trip_collection=$(anyr object list --type collection --filter name="$collection_name" $personal_space | jq -r '.items[0].id')
# get items in collection
trip_objs=$(anyr object get $personal_space $trip_collection --json | jq -r '.properties[] | select(.key=="links") | .objects[]')
# generate csv list of all trips (id,name)
for obj in $trip_objs
  anyr object get $personal_space $obj --json | jq -r '[.id,.name] | @csv'
done
```

### List tasks

```sh
spaceid="bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.3333333333333"
for task in $(anyr object list --type task $spaceid --json | jq -r '.items[] | .id'); do
  data=$(anyr object get $space_id $task --json)
  status=$(jq -r '.properties[] | select (.key=="status") .select.name' <<< "$data")
  name=$(jq -r '.name' <<< "$data")
  created_date=$(jq -r '.properties[] | select (.key=="created_date") .date' <<< "$data" | sed 's/T.*$//')
  printf "%10s %-12s %s" $created_date $status $name
done
```

### List items in a collection

```sh
# get the collection
anyr object get <SPACE_ID> <COLLECTION_ID> --pretty

jq '.properties[] | select(.key == "links") | .objects[]'

# If you want them as an array instead of individual values:
jq '[.properties[] | select(.key == "links") | .objects[]]'

# Or more simply:

jq '.properties[] | select(.key == "links") | .objects'
```

### List items in a query

```
# list queries, look for the query you want (use in <query_id>)
anyr object list --type set <SPACE_ID> -t

# list views of the query (look for view All, get the id <view_id>)
anyr list views <SPACE_ID> <QUERY_ID> -t

# list items in that view
anyr list objects --view $view_id $space_id $query_id
```

### Search

Search for text in title and markdown body, across all spaces the user is authorized to access.

Add the `--space SPACE_ID` arg to limit search to a specific space.

```
anyr search --text "meeting notes"

```

## Output Formats

- `--json` (default)
- `--pretty` (json pretty-print)
- `--table` (readable)
- `--quiet` (minimal output)

## Logging

Debug logging

```sh
RUST_LOG=debug anyr object list <SPACE_ID>
```

Log HTTP request/response:

```sh
RUST_LOG=warn,anytype::http_json=trace anyr object list <SPACE_ID>
```


## Testing

Python CLI tests expect the same environment variables as the API tests:

- `ANYTYPE_TEST_URL` (or `ANYTYPE_URL`)
- `ANYTYPE_TEST_KEY_FILE` (or `ANYTYPE_KEY_FILE`)
- `ANYTYPE_TEST_SPACE_ID`

```sh
source .test-env
python tests/cli_commands.py
```

## License

Licensed under either of:

- Apache License, Version 2.0 (`LICENSE-APACHE`)
- MIT License (`LICENSE-MIT`)
```
````
