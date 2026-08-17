# Export Anytype documents as markdown

To export all documents in a space as markdown:

```shell
anyr backup export \
  --space "My Space" \
  --format markdown \
  --include-properties \
  --include-files \
  --dest ./my-space-markdown.zip
```

Then unpack with

```
unzip ./my-space-markdown.zip -d ./my-space-markdown
```

This uses Anytype's native Markdown exporter, which retains tables and styling info.
`anyr backup export ...` may be useful for importing into another tool, or indexing for an LLM or search tool, but
remember that markdown it is somewhat lossy, because it doesn't retain all of Anytype's block information.

Variations:

- Linked objects and attachments: `--include-nested`, `--include-files`.
- Add property and schema metadata frontmatter: `--include-properties`
- Full space: omit `--objects` and `--types`.
- Specific types: `--types page` or `--types page,note`.
- Explicit IDs: `--objects FILE` or `--objects -`. This can be used to export collections and document by tag.
- Archived objects and backlinks: `--include-archived`, `--include-backlinks`.

The frontmatter is not identical to `anyr md get`, which creates a compact header
containing space_id, object_id, name, created_date, and tags before appending the Markdown body.

## Export items in a collection

```shell
anyr list objects "My Space" "$COLLECTION_ID" --view All --all |
jq -r '.[].id' |
anyr backup export\
--space "My Space"\
--objects -\
--format markdown\
--include-properties\
--include-files\
--dest ./collection.zip
```

## Export items with a tag

```shell
anyr object list "My Space"\
--all\
--type page\
--filter 'tags[in]=urgent' |
jq -r '.[].id' |
anyr backup export\
--space "My Space"\
--objects -\
--format markdown\
--include-properties\
--include-files\
--dest ./urgent-pages.zip
```

## Extract markdown from a protobuf backup

`anyr backup create ...` accepts the same arguments as `anyr backup export` but creates a non-lossy export that can be imported into Anytype app in the same or a different space. Given an archive you an extract individual documents as markdown:

- `anyr backup extract ARCHIVE OBJECT_ID OUTPUT.md` converts one document.
- `anyr backup inspect ARCHIVE` lets you browse and save individual documents.

- The inspector’s save path adds a richer YAML header containing identity, type, timestamps, creator, and
  properties

- Plain backup extract currently uses the simpler renderer and does not add that inspector frontmatter
