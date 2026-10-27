//! A generated product catalog: 200 invented products with warranty and
//! support team. Always the same, so it can serve as a large, stable prompt
//! prefix that provider caches can reuse.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    pub sku: String,
    pub name: String,
    pub category: &'static str,
    pub warranty_months: u32,
    pub team: &'static str,
}

const FAMILIES: [(&str, &str); 6] = [
    ("laptops", "Aurora"),
    ("monitors", "Vista"),
    ("printers", "Inkline"),
    ("routers", "Meshnet"),
    ("phones", "Callpoint"),
    ("docks", "Portline"),
];
const EDITIONS: [&str; 4] = ["Basic", "Plus", "Pro", "Max"];
const WARRANTIES: [u32; 4] = [12, 24, 36, 60];
const PRODUCTS: usize = 200;

pub fn catalog() -> Vec<Product> {
    (0..PRODUCTS)
        .map(|i| {
            let (category, family) = FAMILIES[i % FAMILIES.len()];
            Product {
                sku: format!("NW-{}", 1000 + i),
                name: format!(
                    "{family} {} {}",
                    100 + (i * 37) % 900,
                    EDITIONS[(i / 6) % EDITIONS.len()]
                ),
                category,
                warranty_months: WARRANTIES[(i * 7 + i / 6) % WARRANTIES.len()],
                team: if (i * 5 + i / 3) % 3 == 0 {
                    "marina"
                } else {
                    "joao"
                },
            }
        })
        .collect()
}

pub fn catalog_text() -> String {
    let mut text = String::from(
        "PRODUCT CATALOG\n\nOne line per product: code | name | category | warranty | support team.\n\n",
    );
    for product in catalog() {
        text.push_str(&format!(
            "{} | {} | {} | warranty {} months | support team {}\n",
            product.sku, product.name, product.category, product.warranty_months, product.team
        ));
    }
    text
}

/// The product of ticket 13: catalog team differs from the handbook default.
pub fn routed_product() -> Product {
    catalog()
        .into_iter()
        .skip(150)
        .find(|p| p.team == "marina")
        .unwrap_or_else(|| catalog()[0].clone())
}

/// The product of ticket 14: bought 30 months ago, warranty already over.
pub fn expired_product() -> Product {
    catalog()
        .into_iter()
        .skip(160)
        .find(|p| p.warranty_months < 30 && p.category == "laptops")
        .unwrap_or_else(|| catalog()[0].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::HANDBOOK;

    #[test]
    fn catalog_is_stable_with_unique_codes() {
        assert_eq!(catalog(), catalog());
        let mut codes: Vec<String> = catalog().into_iter().map(|p| p.sku).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), PRODUCTS);
    }

    #[test]
    fn knowledge_prefix_is_above_the_gemini_cache_minimum() {
        // About 4 characters per token; Gemini Flash caches from 4,096 tokens.
        let chars = HANDBOOK.len() + catalog_text().len();
        assert!(
            chars / 4 > 4_600,
            "prefix too small to cache: ~{} tokens",
            chars / 4
        );
    }

    #[test]
    fn trap_products_have_the_properties_the_tasks_need() {
        assert_eq!(routed_product().team, "marina");
        assert!(expired_product().warranty_months < 30);
    }
}
