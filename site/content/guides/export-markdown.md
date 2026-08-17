+++
title = "Export Markdown"
weight = 10
+++

# Export Anytype documents as Markdown

`anyr backup export` uses Anytype's native Markdown exporter. It retains tables
and styling information, can include attachments and property metadata, and can
select a complete space or a narrower object set.

The command examples use `unzip` to extract archives and `jq` to select values
from JSON. Install those tools first, or substitute equivalent archive and JSON
tools on Windows.

Export a complete space:

```sh
anyr backup export \
  --space "My Space" \
  --format markdown \
  --include-properties \
  --include-files \
  --dest ./my-space-markdown.zip
```

Unpack the archive into a directory:

```sh
unzip ./my-space-markdown.zip -d ./my-space-markdown
```

Markdown export works well for moving content into another tool or preparing it
for search and language-model indexing. The format is lossy because it does not
retain every Anytype block detail. Use `anyr backup create` when the archive
must preserve Anytype's protobuf representation for later restoration.

## Select exported content

- Include linked objects and attachments with `--include-nested` and
  `--include-files`.
- Add property and schema metadata as front matter with `--include-properties`.
- Export the complete space by omitting `--objects` and `--types`.
- Select types with `--types page` or `--types page,note`.
- Read explicit object IDs from a file with `--objects FILE`, or from standard
  input with `--objects -`.
- Include archived objects or backlinks with `--include-archived` and
  `--include-backlinks`.

The front matter differs from `anyr md get`. That command writes a compact
header containing the space ID, object ID, name, creation date, and tags before
the Markdown body.

## Export a collection view

Resolve the collection ID, choose one of its views, and send the returned object
IDs to the exporter:

```sh
anyr list objects "My Space" "$COLLECTION_ID" --view All --all \
  | jq -r '.[].id' \
  | anyr backup export \
      --space "My Space" \
      --objects - \
      --format markdown \
      --include-properties \
      --include-files \
      --dest ./collection.zip
```

## Export objects with a tag

Search for the tag through the object's tag property, then stream the matching
IDs into the exporter:

```sh
anyr object list "My Space" \
  --all \
  --type page \
  --filter 'tags[in]=urgent' \
  | jq -r '.[].id' \
  | anyr backup export \
      --space "My Space" \
      --objects - \
      --format markdown \
      --include-properties \
      --include-files \
      --dest ./urgent-pages.zip
```

## Extract Markdown from a protobuf backup

`anyr backup create` accepts the same selection arguments as `backup export`
but creates a lossless archive that Anytype can import into the same or another
space.

Given a backup archive:

- `anyr backup extract ARCHIVE OBJECT_ID OUTPUT.md` converts one document.
- `anyr backup inspect ARCHIVE` opens a terminal interface for browsing and
  saving individual documents.

The inspector adds YAML front matter containing identity, type, timestamps,
creator, and properties. Plain `backup extract` uses the simpler renderer and
does not add the inspector's front matter.
