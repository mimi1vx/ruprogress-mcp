//! `RedmineUP` Products: `GET /products.json`, `GET /projects/{pid}/products.json`,
//! `GET/POST/PUT /products/{id}.json`.
//!
//! Synthetic models derived from the reference implementation's handling of
//! this plugin, not a live capture — Products is commercial. See
//! `tests/fixtures/README.md`'s plugin fixtures section.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::ProjectIdent;
use crate::model::custom_field::{CustomField, CustomFieldWrite};
use crate::model::{Collection, permissive_datetime_opt};

/// Filter for `GET /products.json` / `GET /projects/{pid}/products.json`.
/// `project_id` selects which of the two endpoints
/// [`crate::client::Scoped::list_products`] calls.
#[derive(Debug, Default, Clone)]
pub struct ProductQuery {
    /// Restrict to one project's products, via `GET
    /// /projects/{pid}/products.json`. `None` lists every accessible
    /// product via `GET /products.json`.
    pub project_id: Option<ProjectIdent>,
}

/// A `RedmineUP` product. Every field but `id` is `#[serde(default)]`: the
/// reference implementation has observed plugin versions that omit any of
/// them.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct Product {
    /// The product id.
    #[serde(default)]
    pub id: u64,
    /// The product's display name.
    #[serde(default)]
    pub name: String,
    /// Free-text description.
    #[serde(default)]
    pub description: Option<String>,
    /// A short product code/SKU.
    #[serde(default)]
    pub code: Option<String>,
    /// Unit price.
    #[serde(default)]
    pub price: Option<f64>,
    /// The price's currency, e.g. `"USD"`.
    #[serde(default)]
    pub currency: Option<String>,
    /// `1` = Active, `2` = Inactive.
    #[serde(default)]
    pub status_id: Option<u8>,
    /// The product category id, if any.
    #[serde(default)]
    pub category_id: Option<u64>,
    /// The project this product is associated with, if any.
    #[serde(default)]
    pub project_id: Option<u64>,
    /// Tags attached to the product.
    #[serde(default)]
    pub tag_list: Option<Vec<String>>,
    /// Custom field values attached to the product.
    #[serde(default)]
    pub custom_fields: Option<Vec<CustomField>>,
    /// When the product was created.
    #[serde(default, deserialize_with = "permissive_datetime_opt")]
    pub created_on: Option<DateTime<Utc>>,
    /// When the product was last updated.
    #[serde(default, deserialize_with = "permissive_datetime_opt")]
    pub updated_on: Option<DateTime<Utc>>,
}

/// Payload for `POST /products.json` and `PUT /products/{id}.json`. Every
/// field optional and shared between create and update (the reference's
/// create-parameter set and its update-`fields` allowlist are the same
/// set): only fields set here are sent.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProductWrite {
    /// The product's display name. Required by Redmine on create.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Free-text description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A short product code/SKU.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Unit price.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
    /// The price's currency, e.g. `"USD"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// `1` = Active, `2` = Inactive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_id: Option<u8>,
    /// The product category id, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_id: Option<u64>,
    /// The project to associate this product with, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<u64>,
    /// Replaces the product's full tag set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_list: Option<Vec<String>>,
    /// Custom field values to set, by id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_fields: Option<Vec<CustomFieldWrite>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ProductWriteEnvelope<'a> {
    pub product: &'a ProductWrite,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProductEnvelope {
    pub product: Product,
}

/// `GET /products.json` / `GET /projects/{pid}/products.json`. Both plugins
/// subclass Redmine's own controller machinery and expose `total_count`/
/// `offset`/`limit` on their index actions (R3) — kept as required fields,
/// not `Option`, so a plugin version that omits them is a loud `Decode`
/// error naming the endpoint rather than a silently-presented first page.
#[derive(Debug, Deserialize)]
pub(crate) struct ProductsEnvelope {
    products: Vec<Product>,
    total_count: u64,
    offset: u64,
    limit: u32,
}

impl Collection for ProductsEnvelope {
    type Item = Product;

    fn total_count(&self) -> u64 {
        self.total_count
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    fn limit(&self) -> u32 {
        self.limit
    }

    fn into_items(self) -> Vec<Product> {
        self.products
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    #[test]
    fn product_missing_every_optional_field_still_parses() {
        let json = r#"{"id": 5}"#;
        let product: Product = serde_json::from_str(json).expect("should parse");
        assert_eq!(product.id, 5);
        assert_eq!(product.name, "");
        assert_eq!(product.description, None);
        assert!(product.custom_fields.is_none());
    }

    #[test]
    fn product_with_every_field_parses() {
        let json = r#"{
            "id": 1, "name": "Widget", "description": "A widget", "code": "W-1",
            "price": 9.99, "currency": "USD", "status_id": 1, "category_id": 2,
            "project_id": 3, "tag_list": ["a", "b"],
            "custom_fields": [{"id": 1, "name": "Colour", "value": "blue"}],
            "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-02T00:00:00Z"
        }"#;
        let product: Product = serde_json::from_str(json).expect("should parse");
        assert_eq!(product.name, "Widget");
        assert_eq!(
            product.tag_list,
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(product.custom_fields.expect("custom_fields").len(), 1);
    }

    #[test]
    fn products_envelope_missing_total_count_is_a_decode_error() {
        let json = r#"{"products": []}"#;
        assert!(serde_json::from_str::<ProductsEnvelope>(json).is_err());
    }

    #[test]
    fn write_serializes_only_set_fields() {
        let write = ProductWrite {
            name: Some("Widget".to_string()),
            ..ProductWrite::default()
        };
        let value = serde_json::to_value(ProductWriteEnvelope { product: &write }).unwrap();
        let obj = value
            .get("product")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert_eq!(obj["name"], "Widget");
        assert_eq!(obj.len(), 1);
    }
}
