//! Helpers for working with dataview-based views.

use std::collections::HashMap;
use std::convert::TryFrom;
use std::fmt;

use prost_types::value::Kind;
use tonic::transport::Channel;
use tonic::{Request, Status};

use crate::anytype::ClientCommandsClient;
use crate::anytype::rpc::object::show::Request as ObjectShowRequest;
use crate::auth::with_token;
use crate::model;
use crate::model::block::ContentValue;
use crate::model::block::content::Dataview as BlockDataview;
use crate::model::block::content::dataview;

/// Column metadata for a grid (table) view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridViewColumn {
    pub relation_key: String,
    pub name: String,
    pub format: Option<model::RelationFormat>,
    pub formula: dataview::relation::FormulaType,
    pub is_visible: bool,
    pub width: i32,
}

/// Grid (table) view metadata and columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridViewInfo {
    pub block_id: String,
    pub view_id: String,
    pub view_name: String,
    pub columns: Vec<GridViewColumn>,
}

/// Errors returned when loading view metadata.
#[derive(Debug)]
pub enum ViewError {
    Transport(Status),
    Api { code: i32, description: String },
    MissingObjectView,
    MissingDataviewBlock { view_id: String },
    MissingView { view_id: String },
    NotSupportedView { view_id: String, actual: i32 },
}

impl fmt::Display for ViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ViewError::Transport(status) => write!(f, "transport error: {status}"),
            ViewError::Api { code, description } => {
                write!(f, "api error {code}: {description}")
            }
            ViewError::MissingObjectView => write!(f, "object view missing in response"),
            ViewError::MissingDataviewBlock { view_id } => {
                write!(f, "dataview block not found for view id {view_id}")
            }
            ViewError::MissingView { view_id } => write!(f, "view id {view_id} not found"),
            ViewError::NotSupportedView { view_id, actual } => write!(
                f,
                "view id {view_id} is not a supported view (type {actual})"
            ),
        }
    }
}

impl std::error::Error for ViewError {}

impl From<Status> for ViewError {
    fn from(status: Status) -> Self {
        ViewError::Transport(status)
    }
}

/// Fetch table/list view column metadata for a type object.
pub async fn fetch_grid_view_columns(
    client: &mut ClientCommandsClient<Channel>,
    token: &str,
    space_id: &str,
    type_id: &str,
    view_id: &str,
) -> Result<GridViewInfo, ViewError> {
    let request = ObjectShowRequest {
        object_id: type_id.to_string(),
        space_id: space_id.to_string(),
        include_relations_as_dependent_objects: true,
        ..Default::default()
    };
    let request = with_token(Request::new(request), token).map_err(|err| ViewError::Api {
        code: 0,
        description: err.to_string(),
    })?;

    let response = client.object_show(request).await?.into_inner();
    if let Some(error) = response.error
        && error.code != 0
    {
        return Err(ViewError::Api {
            code: error.code,
            description: error.description,
        });
    }

    let object_view = response.object_view.ok_or(ViewError::MissingObjectView)?;
    let relation_names = relation_name_index(&object_view.details);

    let (block_id, dataview) = find_dataview_block(&object_view.blocks, view_id)?;
    let view = dataview
        .views
        .iter()
        .find(|view| view.id == view_id)
        .ok_or_else(|| ViewError::MissingView {
            view_id: view_id.to_string(),
        })?;

    let view_type =
        dataview::view::Type::try_from(view.r#type).unwrap_or(dataview::view::Type::Table);
    if view_type != dataview::view::Type::Table && view_type != dataview::view::Type::List {
        return Err(ViewError::NotSupportedView {
            view_id: view_id.to_string(),
            actual: view.r#type,
        });
    }

    let relation_formats = relation_format_index(&dataview);
    let columns = view
        .relations
        .iter()
        .map(|relation| {
            let formula = dataview::relation::FormulaType::try_from(relation.formula)
                .unwrap_or(dataview::relation::FormulaType::None);
            let name = relation_names
                .get(&relation.key)
                .cloned()
                .unwrap_or_else(|| relation.key.clone());
            let format = relation_formats.get(&relation.key).cloned();

            GridViewColumn {
                relation_key: relation.key.clone(),
                name,
                format,
                formula,
                is_visible: relation.is_visible,
                width: relation.width,
            }
        })
        .collect::<Vec<_>>();

    Ok(GridViewInfo {
        block_id,
        view_id: view.id.clone(),
        view_name: view.name.clone(),
        columns,
    })
}

fn find_dataview_block(
    blocks: &[model::Block],
    view_id: &str,
) -> Result<(String, BlockDataview), ViewError> {
    for block in blocks {
        if let Some(ContentValue::Dataview(dataview)) = block.content_value.as_ref()
            && dataview.views.iter().any(|view| view.id == view_id)
        {
            return Ok((block.id.clone(), dataview.clone()));
        }
    }

    Err(ViewError::MissingDataviewBlock {
        view_id: view_id.to_string(),
    })
}

fn relation_format_index(dataview: &BlockDataview) -> HashMap<String, model::RelationFormat> {
    let mut map = HashMap::new();
    for link in &dataview.relation_links {
        if let Ok(format) = model::RelationFormat::try_from(link.format) {
            map.insert(link.key.clone(), format);
        }
    }
    map
}

fn relation_name_index(details: &[model::object_view::DetailsSet]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for detail_set in details {
        let Some(details) = detail_set.details.as_ref() else {
            continue;
        };
        let relation_key =
            string_field(details, "relationKey").or_else(|| string_field(details, "key"));
        let name = string_field(details, "name");
        if let (Some(relation_key), Some(name)) = (relation_key, name) {
            map.insert(relation_key, name);
        }
    }
    map
}

fn string_field(details: &prost_types::Struct, key: &str) -> Option<String> {
    details.fields.get(key).and_then(|value| match &value.kind {
        Some(Kind::StringValue(value)) => Some(value.clone()),
        _ => None,
    })
}
