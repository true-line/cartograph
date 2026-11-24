use super::*;

const MAIN_TF: &str = r#"
resource "aws_sqs_queue" "orders" {
  name = "orders-queue"
}

resource "aws_lambda_function" "fulfill" {
  function_name = "fulfill-orders"
  role          = aws_iam_role.lambda_role.arn
}

resource "aws_lambda_event_source_mapping" "orders_to_fulfill" {
  event_source_arn = aws_sqs_queue.orders.arn
  function_name    = aws_lambda_function.fulfill.arn
}

resource "aws_iam_role" "lambda_role" {
  name = "fulfill-role"
}

resource "aws_iam_role_policy" "fulfill_policy" {
  role   = aws_iam_role.lambda_role.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Action   = ["sqs:ReceiveMessage", "sqs:DeleteMessage"]
      Effect   = "Allow"
      Resource = aws_sqs_queue.orders.arn
    }]
  })
}

resource "aws_sns_topic_subscription" "alerts" {
  topic_arn = aws_sns_topic.alerts.arn
  protocol  = "sqs"
  endpoint  = aws_sqs_queue.orders.arn

  depends_on = [aws_sqs_queue.orders]
}

resource "aws_sns_topic" "alerts" {
  name = "alerts"
}

module "vpc" {
  source = "./modules/vpc"
}

data "aws_ami" "app" {
  owners = ["self"]
}

resource "aws_instance" "app" {
  ami       = data.aws_ami.app.id
  subnet_id = module.vpc.private_subnet_id
  count     = var.instance_count
}
"#;

fn id() -> SourceId<'static> {
    SourceId {
        repo: "qwtm/infra",
        commit: "def456",
    }
}

fn edge_pairs<'a>(ex: &'a Extraction, label: &str) -> Vec<(&'a str, &'a str)> {
    ex.edges
        .iter()
        .filter(|e| e.label == label)
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect()
}

#[test]
fn resources_data_and_modules_become_resource_nodes() {
    // AC-0007: resource DAG built from HCL.
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let ids: Vec<_> = ex.nodes.iter().map(|n| n.id.as_str()).collect();
    assert!(ids.contains(&"res:qwtm/infra@aws_sqs_queue.orders"));
    assert!(ids.contains(&"res:qwtm/infra@data.aws_ami.app"));
    assert!(ids.contains(&"res:qwtm/infra@module.vpc"));
    let queue = ex
        .nodes
        .iter()
        .find(|n| n.id == "res:qwtm/infra@aws_sqs_queue.orders")
        .unwrap();
    assert_eq!(queue.props["provider"], "aws");
    assert_eq!(queue.props["type"], "aws_sqs_queue");
}

#[test]
fn interpolation_references_build_the_dag() {
    // AC-0007: refs like data.aws_ami.app.id / module.vpc.x become edges;
    // var.* / count.* never do.
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let refs = edge_pairs(&ex, "REFERENCES");
    assert!(refs.contains(&(
        "res:qwtm/infra@aws_instance.app",
        "res:qwtm/infra@data.aws_ami.app"
    )));
    assert!(refs.contains(&(
        "res:qwtm/infra@aws_instance.app",
        "res:qwtm/infra@module.vpc"
    )));
    assert!(refs.contains(&(
        "res:qwtm/infra@aws_lambda_function.fulfill",
        "res:qwtm/infra@aws_iam_role.lambda_role"
    )));
    assert!(
        !ex.edges
            .iter()
            .any(|e| e.dst.contains("var.") || e.dst.contains("count."))
    );
}

#[test]
fn depends_on_is_distinct_from_references() {
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let deps = edge_pairs(&ex, "DEPENDS_ON");
    assert!(deps.contains(&(
        "res:qwtm/infra@aws_sns_topic_subscription.alerts",
        "res:qwtm/infra@aws_sqs_queue.orders"
    )));
}

#[test]
fn capability_registry_emits_triggers_deterministically() {
    // AC-0008: the event source mapping mediates SQS -> Lambda TRIGGERS.
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let triggers = edge_pairs(&ex, "TRIGGERS");
    assert_eq!(
        triggers,
        vec![(
            "res:qwtm/infra@aws_sqs_queue.orders",
            "res:qwtm/infra@aws_lambda_function.fulfill"
        )]
    );
    let edge = ex.edges.iter().find(|e| e.label == "TRIGGERS").unwrap();
    assert_eq!(
        edge.props["via"],
        "res:qwtm/infra@aws_lambda_event_source_mapping.orders_to_fulfill"
    );
    assert_eq!(edge.props["registry"], registry::REGISTRY_VERSION);
}

#[test]
fn capability_registry_emits_subscribes() {
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let subs = edge_pairs(&ex, "SUBSCRIBES");
    assert_eq!(
        subs,
        vec![(
            "res:qwtm/infra@aws_sqs_queue.orders",
            "res:qwtm/infra@aws_sns_topic.alerts"
        )]
    );
}

#[test]
fn capability_registry_routes_nested_lb_listener_action() {
    // AC-0008: the selector refactor preserves refs inside a nested target
    // block for the original ALB registry entry.
    let source = r#"
resource "aws_lb" "public" {}
resource "aws_lb_target_group" "app" {}
resource "aws_lb_listener" "https" {
  load_balancer_arn = aws_lb.public.arn
  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.app.arn
  }
}
"#;
    let ex = extract_source(source, "alb.tf", &id()).unwrap();
    assert_eq!(
        edge_pairs(&ex, "ROUTES"),
        vec![(
            "res:qwtm/infra@aws_lb.public",
            "res:qwtm/infra@aws_lb_target_group.app"
        )]
    );
}

#[test]
fn capability_registry_routes_api_gateway_integrations() {
    // AC-0043: API Gateway v1 integration mediates REST API -> target ROUTES.
    let source = r#"
resource "aws_api_gateway_rest_api" "orders" {}
resource "aws_lambda_function" "handler" {}
resource "aws_api_gateway_integration" "orders" {
  rest_api_id = aws_api_gateway_rest_api.orders.id
  uri         = aws_lambda_function.handler.invoke_arn
}
"#;
    let ex = extract_source(source, "api.tf", &id()).unwrap();
    assert_eq!(
        edge_pairs(&ex, "ROUTES"),
        vec![(
            "res:qwtm/infra@aws_api_gateway_rest_api.orders",
            "res:qwtm/infra@aws_lambda_function.handler"
        )]
    );
    let edge = ex.edges.iter().find(|edge| edge.label == "ROUTES").unwrap();
    assert_eq!(
        edge.props["via"],
        "res:qwtm/infra@aws_api_gateway_integration.orders"
    );
    assert_eq!(edge.props["registry"], registry::REGISTRY_VERSION);
}

#[test]
fn capability_registry_triggers_through_lambda_permissions() {
    // AC-0044: a permission's source ARN invokes its Lambda function.
    let source = r#"
resource "aws_cloudwatch_event_rule" "nightly" {}
resource "aws_lambda_function" "worker" {}
resource "aws_lambda_permission" "nightly" {
  source_arn    = aws_cloudwatch_event_rule.nightly.arn
  function_name = aws_lambda_function.worker.function_name
}
"#;
    let ex = extract_source(source, "permission.tf", &id()).unwrap();
    assert_eq!(
        edge_pairs(&ex, "TRIGGERS"),
        vec![(
            "res:qwtm/infra@aws_cloudwatch_event_rule.nightly",
            "res:qwtm/infra@aws_lambda_function.worker"
        )]
    );
}

#[test]
fn capability_registry_triggers_lambda_at_edge_from_nested_cache_behaviors() {
    // AC-0045: both CloudFront cache-behavior forms select nested Lambda ARNs;
    // unrelated nested blocks must not broaden the deterministic match.
    let source = r#"
resource "aws_lambda_function" "viewer_request" {}
resource "aws_lambda_function" "origin_response" {}
resource "aws_lambda_function" "unrelated" {}
resource "aws_cloudfront_distribution" "site" {
  default_cache_behavior {
    lambda_function_association {
      event_type = "viewer-request"
      lambda_arn = aws_lambda_function.viewer_request.qualified_arn
    }
  }
  ordered_cache_behavior {
    lambda_function_association {
      event_type = "origin-response"
      lambda_arn = aws_lambda_function.origin_response.qualified_arn
    }
  }
  origin {
    lambda_arn = aws_lambda_function.unrelated.arn
  }
}
"#;
    let ex = extract_source(source, "cloudfront.tf", &id()).unwrap();
    let triggers = edge_pairs(&ex, "TRIGGERS");
    assert_eq!(triggers.len(), 2);
    assert!(triggers.contains(&(
        "res:qwtm/infra@aws_cloudfront_distribution.site",
        "res:qwtm/infra@aws_lambda_function.viewer_request"
    )));
    assert!(triggers.contains(&(
        "res:qwtm/infra@aws_cloudfront_distribution.site",
        "res:qwtm/infra@aws_lambda_function.origin_response"
    )));
    assert!(!triggers.iter().any(|(_, dst)| dst.contains("unrelated")));
}

#[test]
fn capability_registry_triggers_aws_pipes() {
    // AC-0046: EventBridge Pipes deterministically connect source -> target.
    let source = r#"
resource "aws_sqs_queue" "orders" {}
resource "aws_lambda_function" "dispatch" {}
resource "aws_pipes_pipe" "orders" {
  source = aws_sqs_queue.orders.arn
  target = aws_lambda_function.dispatch.arn
}
"#;
    let ex = extract_source(source, "pipes.tf", &id()).unwrap();
    assert_eq!(
        edge_pairs(&ex, "TRIGGERS"),
        vec![(
            "res:qwtm/infra@aws_sqs_queue.orders",
            "res:qwtm/infra@aws_lambda_function.dispatch"
        )]
    );
}

#[test]
fn iam_policy_grants_reference_target_resources_with_actions() {
    // AC-0008 (GRANTS) — feeds the security view (US-0015).
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let grants = edge_pairs(&ex, "GRANTS");
    assert_eq!(
        grants,
        vec![(
            "res:qwtm/infra@aws_iam_role_policy.fulfill_policy",
            "res:qwtm/infra@aws_sqs_queue.orders"
        )]
    );
    let edge = ex.edges.iter().find(|e| e.label == "GRANTS").unwrap();
    let actions: Vec<String> = serde_json::from_value(edge.props["actions"].clone()).unwrap();
    assert_eq!(actions, vec!["sqs:DeleteMessage", "sqs:ReceiveMessage"]);
}

#[test]
fn iam_wildcard_actions_and_scopes_remain_visible() {
    // AC-0042 (T-0042): deterministic extraction must retain the exact
    // wildcard inputs that drive the security projection.
    let source = r#"
data "aws_iam_policy_document" "admin" {
  statement {
    actions   = ["*"]
    resources = ["*"]
  }
}

resource "aws_iam_policy" "admin" {
  policy = data.aws_iam_policy_document.admin.json
}
"#;
    let ex = extract_source(source, "admin.tf", &id()).unwrap();
    let grant = ex.edges.iter().find(|edge| edge.label == "GRANTS").unwrap();
    assert_eq!(grant.props["actions"], serde_json::json!(["*"]));
    assert_eq!(grant.props["resource_scopes"], serde_json::json!(["*"]));
    assert_eq!(
        grant.dst,
        "res:qwtm/infra@data.aws_iam_policy_document.admin"
    );

    let scope_only = source.replace("actions   = [\"*\"]", "actions   = [\"s3:GetObject\"]");
    let scope_only = extract_source(&scope_only, "scope.tf", &id()).unwrap();
    let grant = scope_only
        .edges
        .iter()
        .find(|edge| edge.label == "GRANTS")
        .unwrap();
    assert_eq!(grant.props["actions"], serde_json::json!(["s3:GetObject"]));
    assert_eq!(grant.props["resource_scopes"], serde_json::json!(["*"]));

    // Each resolved edge carries only the actions from statements that name
    // its resource; NotAction must never be parsed as Action.
    let statement_scoped = r#"
resource "aws_s3_bucket" "admin" {}
resource "aws_s3_bucket" "uploads" {}

data "aws_iam_policy_document" "mixed" {
  statement {
    actions   = ["*"]
    resources = [aws_s3_bucket.admin.arn]
  }
  statement {
    actions   = ["s3:GetObject"]
    resources = [aws_s3_bucket.uploads.arn]
  }
  statement {
    not_actions = ["*"]
    resources   = [aws_s3_bucket.uploads.arn]
  }
}

resource "aws_iam_policy" "mixed" {
  policy = data.aws_iam_policy_document.mixed.json
}
"#;
    let scoped = extract_source(statement_scoped, "mixed.tf", &id()).unwrap();
    let grants = scoped
        .edges
        .iter()
        .filter(|edge| edge.label == "GRANTS")
        .collect::<Vec<_>>();
    assert_eq!(grants.len(), 2);
    let admin = grants
        .iter()
        .find(|edge| edge.dst.ends_with("aws_s3_bucket.admin"))
        .unwrap();
    assert_eq!(admin.props["actions"], serde_json::json!(["*"]));
    let uploads = grants
        .iter()
        .find(|edge| edge.dst.ends_with("aws_s3_bucket.uploads"))
        .unwrap();
    assert_eq!(
        uploads.props["actions"],
        serde_json::json!(["s3:GetObject"])
    );
}

#[test]
fn iam_policy_grants_chase_same_extraction_policy_document() {
    // AC-0047: a defined policy document is an intermediate contract, not the
    // terminal resource granted by the IAM policy.
    let source = r#"
resource "aws_sqs_queue" "orders" {}
resource "aws_kms_key" "orders" {}
resource "aws_s3_bucket" "orders" {}

data "aws_iam_policy_document" "orders" {
  statement {
    actions = ["sqs:SendMessage", "kms:Decrypt"]
    resources = [
      aws_sqs_queue.orders.arn,
      aws_kms_key.orders.arn,
    ]
  }
  dynamic "statement" {
    for_each = [1]
    content {
      actions   = ["kms:GenerateDataKey"]
      resources = [aws_s3_bucket.orders.arn]
    }
  }
}

resource "aws_iam_policy" "orders" {
  policy = data.aws_iam_policy_document.orders.json
}
"#;
    let ex = extract_source(source, "iam.tf", &id()).unwrap();
    let grants = edge_pairs(&ex, "GRANTS");
    assert_eq!(grants.len(), 3);
    assert!(grants.contains(&(
        "res:qwtm/infra@aws_iam_policy.orders",
        "res:qwtm/infra@aws_sqs_queue.orders"
    )));
    assert!(grants.contains(&(
        "res:qwtm/infra@aws_iam_policy.orders",
        "res:qwtm/infra@aws_kms_key.orders"
    )));
    assert!(grants.contains(&(
        "res:qwtm/infra@aws_iam_policy.orders",
        "res:qwtm/infra@aws_s3_bucket.orders"
    )));
    assert!(
        !grants
            .iter()
            .any(|(_, dst)| dst.contains("policy_document"))
    );
    for edge in ex.edges.iter().filter(|edge| edge.label == "GRANTS") {
        let actions: Vec<String> = serde_json::from_value(edge.props["actions"].clone()).unwrap();
        if edge.dst.ends_with("aws_s3_bucket.orders") {
            assert_eq!(actions, vec!["kms:GenerateDataKey"]);
        } else {
            assert_eq!(actions, vec!["kms:Decrypt", "sqs:SendMessage"]);
        }
        assert_eq!(
            edge.props["policy_document"],
            "res:qwtm/infra@data.aws_iam_policy_document.orders"
        );
        assert_eq!(edge.props["prov"]["evidence"].as_array().unwrap().len(), 2);
    }
}

#[test]
fn iam_policy_document_chase_spans_files_in_directory() {
    // AC-0047: "same extraction" covers the whole deterministic directory
    // walk, not only blocks that happen to share one .tf file.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("policy.tf"),
        r#"resource "aws_iam_policy" "orders" {
  policy = data.aws_iam_policy_document.orders.json
}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("document.tf"),
        r#"data "aws_iam_policy_document" "orders" {
  statement {
    actions   = ["sqs:SendMessage"]
    resources = [aws_sqs_queue.orders.arn]
  }
}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("queue.tf"),
        r#"resource "aws_sqs_queue" "orders" {}"#,
    )
    .unwrap();

    let ex = extract_dir(dir.path(), &id()).unwrap();
    let grant = ex.edges.iter().find(|edge| edge.label == "GRANTS").unwrap();
    assert_eq!(grant.src, "res:qwtm/infra@aws_iam_policy.orders");
    assert_eq!(grant.dst, "res:qwtm/infra@aws_sqs_queue.orders");
    let evidence = grant.props["prov"]["evidence"].as_array().unwrap();
    let paths: BTreeSet<_> = evidence
        .iter()
        .map(|item| item["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, BTreeSet::from(["document.tf", "policy.tf"]));
}

#[test]
fn missing_or_unresolved_policy_document_keeps_explicit_grant() {
    // AC-0047: no definition or no resolvable statement resource must never
    // make the policy relationship disappear.
    let missing_source = r#"resource "aws_iam_policy" "orders" {
  policy = data.aws_iam_policy_document.orders.json
}"#;
    let missing = extract_source(missing_source, "missing.tf", &id()).unwrap();
    assert_eq!(
        edge_pairs(&missing, "GRANTS"),
        vec![(
            "res:qwtm/infra@aws_iam_policy.orders",
            "res:qwtm/infra@data.aws_iam_policy_document.orders"
        )]
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("missing.tf"), missing_source).unwrap();
    let closed = extract_dir(dir.path(), &id()).unwrap();
    let placeholder = closed
        .nodes
        .iter()
        .find(|node| node.id == "res:qwtm/infra@data.aws_iam_policy_document.orders")
        .unwrap();
    assert_eq!(placeholder.props["placeholder"], true);
    // AC-0207 (#237): the placeholder is an explicit Gap citing the GRANTS
    // reference that named it, never a fact without provenance.
    assert_eq!(placeholder.props["boundary"], "unresolved");
    assert_eq!(placeholder.props["prov"]["confidence_tier"], "Gap");
    assert_eq!(
        placeholder.props["prov"]["extractor_id"],
        "t0.iac-terraform"
    );
    assert_eq!(
        placeholder.props["prov"]["evidence"][0]["path"],
        "missing.tf"
    );

    let unresolved = extract_source(
        r#"
data "aws_iam_policy_document" "orders" {
  statement {
    actions   = ["sqs:SendMessage"]
    resources = var.sqs_target_arns
  }
}
resource "aws_iam_policy" "orders" {
  policy = data.aws_iam_policy_document.orders.json
}
"#,
        "unresolved.tf",
        &id(),
    )
    .unwrap();
    assert_eq!(
        edge_pairs(&unresolved, "GRANTS"),
        vec![(
            "res:qwtm/infra@aws_iam_policy.orders",
            "res:qwtm/infra@data.aws_iam_policy_document.orders"
        )]
    );
    let grant = unresolved
        .edges
        .iter()
        .find(|edge| edge.label == "GRANTS")
        .unwrap();
    assert_eq!(
        grant.props["actions"],
        serde_json::json!(["sqs:SendMessage"])
    );
    assert_eq!(grant.props["prov"]["evidence"].as_array().unwrap().len(), 2);
}

#[test]
fn every_fact_carries_confirmed_t0_provenance_with_spans() {
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    for props in ex
        .nodes
        .iter()
        .map(|n| &n.props)
        .chain(ex.edges.iter().map(|e| &e.props))
    {
        let prov = props.get("prov").expect("prov present");
        assert_eq!(prov["tier"], "Deterministic");
        assert_eq!(prov["confidence_tier"], "Confirmed");
        assert_eq!(prov["extractor_id"], "t0.iac-terraform");
        let ev = &prov["evidence"][0];
        assert_eq!(ev["path"], "main.tf");
        assert!(ev["byte_end"].as_u64().unwrap() > ev["byte_start"].as_u64().unwrap());
    }
}

#[test]
fn evidence_span_covers_the_declaring_block() {
    let ex = extract_source(MAIN_TF, "main.tf", &id()).unwrap();
    let mapping = ex
        .nodes
        .iter()
        .find(|n| n.id == "res:qwtm/infra@aws_lambda_event_source_mapping.orders_to_fulfill")
        .unwrap();
    let ev = &mapping.props["prov"]["evidence"][0];
    let span = &MAIN_TF
        [ev["byte_start"].as_u64().unwrap() as usize..ev["byte_end"].as_u64().unwrap() as usize];
    assert!(span.contains(r#"resource "aws_lambda_event_source_mapping" "orders_to_fulfill""#));
    assert!(span.contains("event_source_arn = aws_sqs_queue.orders.arn"));
}

#[test]
fn dir_walk_is_deterministic_and_skips_dot_terraform() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.tf"), MAIN_TF).unwrap();
    std::fs::create_dir_all(dir.path().join(".terraform/modules")).unwrap();
    std::fs::write(
        dir.path().join(".terraform/modules/cached.tf"),
        r#"resource "aws_s3_bucket" "cached" {}"#,
    )
    .unwrap();
    let a = extract_dir(dir.path(), &id()).unwrap();
    let b = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(terraform_file_count(dir.path()).unwrap(), 1);
    assert!(!a.nodes.iter().any(|n| n.id.contains("cached")));
    assert_eq!(
        serde_json::to_string(&a.nodes).unwrap(),
        serde_json::to_string(&b.nodes).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&a.edges).unwrap(),
        serde_json::to_string(&b.edges).unwrap()
    );
}

#[test]
fn delta_reingest_reuses_unchanged_terraform_contexts() {
    // AC-0040 (T-0040): unchanged Terraform parses are reused by byte hash.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("queue.tf"),
        r#"resource "aws_sqs_queue" "orders" {}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("topic.tf"),
        r#"resource "aws_sns_topic" "events" {}"#,
    )
    .unwrap();
    let mut cache = IncrementalCache::default();
    let (_, first) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(first.recomputed_files, 2);

    let (_, same) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(same.recomputed_files, 0);
    assert_eq!(same.reused_files, 2);

    std::fs::write(
        dir.path().join("topic.tf"),
        r#"resource "aws_sns_topic" "alerts" {}"#,
    )
    .unwrap();
    let (changed, delta) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(delta.recomputed_files, 1);
    assert_eq!(delta.reused_files, 1);
    assert!(
        changed
            .nodes
            .iter()
            .any(|node| node.id.ends_with(".alerts"))
    );
    assert!(
        !changed
            .nodes
            .iter()
            .any(|node| node.id.ends_with(".events"))
    );

    std::fs::remove_file(dir.path().join("queue.tf")).unwrap();
    let (_, deleted) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(deleted.deleted_files, 1);
}

#[test]
fn distinct_files_dedups_a_module_instantiated_twice() {
    // A local module directory instantiated by two module blocks is parsed
    // as two contexts (one per module address), but it is one physical
    // file: `distinct_files` must count it once so it lines up with
    // `metrics::compute`'s `files_with_facts`, which dedups by provenance
    // `repo:path` (#468).
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("modules/queue")).unwrap();
    std::fs::write(
        dir.path().join("main.tf"),
        r#"
module "orders" { source = "./modules/queue" }
module "alerts" { source = "./modules/queue" }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("modules/queue/main.tf"),
        r#"resource "aws_sqs_queue" "q" {}"#,
    )
    .unwrap();

    let mut cache = IncrementalCache::default();
    let (_, stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    // Four contexts (the plain tree walk finds modules/queue/main.tf once,
    // plus once more per module address that instantiates it, plus root)...
    assert_eq!(stats.recomputed_files, 4);
    // ...but only two physical files.
    assert_eq!(stats.distinct_files, 2);
}

#[test]
fn pulumi_constructor_names_share_terraform_registry_types() {
    // AC-0051/T-0051: provider naming differences are explicit deterministic
    // normalization before both IaC sources consult one registry.
    assert_eq!(
        registry::terraform_type_for_pulumi("@pulumi/aws", "aws.lambda.EventSourceMapping")
            .as_deref(),
        Some("aws_lambda_event_source_mapping")
    );
    assert_eq!(
        registry::pulumi_token_for_constructor("@pulumi/aws", "aws.lambda.EventSourceMapping")
            .as_deref(),
        Some("aws:lambda/eventSourceMapping:EventSourceMapping")
    );
    assert_eq!(
        registry::terraform_type_for_pulumi("@pulumi/aws", "aws.apigateway.Integration").as_deref(),
        Some("aws_api_gateway_integration")
    );
    assert_eq!(
        registry::terraform_type_for_pulumi("@pulumi/aws/s3", "s3.Bucket").as_deref(),
        Some("aws_s3_bucket")
    );
    assert_eq!(
        registry::terraform_type_for_pulumi("@pulumi/aws/s3", "Bucket").as_deref(),
        Some("aws_s3_bucket")
    );
}

#[test]
fn local_modules_expand_under_scoped_addresses_and_edges() {
    // AC-0048: mirrors examples/with-pipes -> source "../../". The root
    // module's resources and semantic edges are instantiated below the module
    // node rather than leaving it as an opaque leaf.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("main.tf"),
        r#"
resource "aws_sqs_queue" "orders" {}
resource "aws_lambda_function" "fulfill" {}
resource "aws_lambda_event_source_mapping" "orders" {
  event_source_arn = aws_sqs_queue.orders.arn
  function_name    = aws_lambda_function.fulfill.arn
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("iam.tf"),
        r#"
data "aws_iam_policy_document" "orders" {
  statement {
    actions   = ["sqs:SendMessage"]
    resources = [aws_sqs_queue.orders.arn]
  }
}
resource "aws_iam_policy" "orders" {
  policy = data.aws_iam_policy_document.orders.json
}
"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("examples/with-pipes")).unwrap();
    std::fs::write(
        dir.path().join("examples/with-pipes/main.tf"),
        r#"module "eventbridge" {
  source = "../../"
}"#,
    )
    .unwrap();

    let ex = extract_dir(dir.path(), &id()).unwrap();
    let ids: BTreeSet<_> = ex.nodes.iter().map(|node| node.id.as_str()).collect();
    assert!(ids.contains("res:qwtm/infra@examples/with-pipes/module.eventbridge"));
    assert!(
        ids.contains("res:qwtm/infra@examples/with-pipes/module.eventbridge.aws_sqs_queue.orders")
    );
    assert!(ids.contains(
        "res:qwtm/infra@examples/with-pipes/module.eventbridge.aws_lambda_function.fulfill"
    ));
    assert!(
        ids.contains("res:qwtm/infra@examples/with-pipes/module.eventbridge.aws_iam_policy.orders")
    );

    let contains = ex
        .edges
        .iter()
        .find(|edge| {
            edge.src == "res:qwtm/infra@examples/with-pipes/module.eventbridge"
                && edge.dst
                    == "res:qwtm/infra@examples/with-pipes/module.eventbridge.aws_sqs_queue.orders"
                && edge.label == "REFERENCES"
                && edge.props["relation"] == "MODULE_CONTAINS"
        })
        .unwrap();
    assert_eq!(contains.props["prov"]["tier"], "Deterministic");
    assert_eq!(contains.props["prov"]["confidence_tier"], "Confirmed");
    assert_eq!(
        contains.props["prov"]["evidence"][0]["path"],
        "examples/with-pipes/main.tf"
    );
    assert!(ex.edges.iter().any(|edge| {
        edge.src == "res:qwtm/infra@examples/with-pipes/module.eventbridge.aws_sqs_queue.orders"
            && edge.dst == "res:qwtm/infra@examples/with-pipes/module.eventbridge.aws_lambda_function.fulfill"
            && edge.label == "TRIGGERS"
    }));
    assert!(ex.edges.iter().any(|edge| {
        edge.src == "res:qwtm/infra@examples/with-pipes/module.eventbridge.aws_iam_policy.orders"
            && edge.dst
                == "res:qwtm/infra@examples/with-pipes/module.eventbridge.aws_sqs_queue.orders"
            && edge.label == "GRANTS"
    }));
}

#[test]
fn nested_local_modules_stop_at_cycles_deterministically() {
    // AC-0048: nested local modules recurse, but outer -> inner -> outer is
    // left as a leaf at the repeated directory instead of looping forever.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("entry")).unwrap();
    std::fs::create_dir_all(dir.path().join("modules/outer")).unwrap();
    std::fs::create_dir_all(dir.path().join("modules/inner")).unwrap();
    std::fs::write(
        dir.path().join("entry/main.tf"),
        r#"module "outer" { source = "../modules/outer" }"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("modules/outer/main.tf"),
        r#"
resource "aws_s3_bucket" "outer" {}
module "inner" { source = "../inner" }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("modules/inner/main.tf"),
        r#"
resource "aws_sqs_queue" "inner" {}
module "outer" { source = "../outer" }
"#,
    )
    .unwrap();

    let first = extract_dir(dir.path(), &id()).unwrap();
    let second = extract_dir(dir.path(), &id()).unwrap();
    let ids: BTreeSet<_> = first.nodes.iter().map(|node| node.id.as_str()).collect();
    assert!(ids.contains("res:qwtm/infra@entry/module.outer.aws_s3_bucket.outer"));
    assert!(ids.contains("res:qwtm/infra@entry/module.outer.module.inner.aws_sqs_queue.inner"));
    assert!(ids.contains("res:qwtm/infra@entry/module.outer.module.inner.module.outer"));
    assert!(
        !ids.iter()
            .any(|id| { id.contains("module.outer.module.inner.module.outer.aws_s3_bucket") })
    );
    assert_eq!(
        serde_json::to_string(&first.nodes).unwrap(),
        serde_json::to_string(&second.nodes).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&first.edges).unwrap(),
        serde_json::to_string(&second.edges).unwrap()
    );
}

#[test]
fn remote_outside_and_symlinked_modules_remain_leaf_nodes() {
    // AC-0048: non-local and out-of-root sources are never read.
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("secret.tf"),
        r#"resource "aws_s3_bucket" "must_not_be_read" {}"#,
    )
    .unwrap();
    std::fs::write(
        root.path().join("main.tf"),
        format!(
            r#"
module "outside" {{ source = "{}" }}
module "remote" {{ source = "terraform-aws-modules/eventbridge/aws" }}
"#,
            // An HCL string escapes a Windows path's backslashes (#227).
            outside.path().display().to_string().replace('\\', "\\\\")
        ),
    )
    .unwrap();

    #[cfg(unix)]
    {
        std::fs::create_dir_all(root.path().join("modules")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("modules/escape")).unwrap();
        std::fs::write(
            root.path().join("symlink.tf"),
            r#"module "symlink" { source = "./modules/escape" }"#,
        )
        .unwrap();
    }

    let ex = extract_dir(root.path(), &id()).unwrap();
    assert!(
        ex.nodes
            .iter()
            .any(|node| node.id == "res:qwtm/infra@module.outside")
    );
    assert!(
        ex.nodes
            .iter()
            .any(|node| node.id == "res:qwtm/infra@module.remote")
    );
    assert!(
        !ex.nodes
            .iter()
            .any(|node| node.id.contains("must_not_be_read"))
    );
    assert!(
        !ex.edges.iter().any(|edge| {
            edge.props.get("relation") == Some(&serde_json::json!("MODULE_CONTAINS"))
        })
    );
}

#[test]
fn syntax_errors_name_the_file() {
    let err = extract_source("resource \"broken\" {", "bad.tf", &id()).unwrap_err();
    assert!(err.to_string().contains("bad.tf"));
}

#[test]
fn gitignored_trees_are_not_collected() {
    // AC-0205 (#248): the shared walk honors the tree's own `.gitignore`.
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in [
        (".gitignore", "gen/\n"),
        ("infra/main.tf", ""),
        ("gen/generated.tf", ""),
    ] {
        let file = dir.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body).unwrap();
    }
    let mut files = Vec::new();
    collect_tf_files(dir.path(), &mut files).unwrap();
    assert_eq!(files, ["infra/main.tf"]);
}

#[test]
fn iac_canonicalizes_through_the_prefix_free_boundary() {
    // AC-0223 (#341): the app hands iac roots canonicalized without the
    // Windows `\\?\` verbatim prefix (`src-tauri/src/paths.rs`). If iac itself
    // called `std::fs::canonicalize`, its roots would be verbatim on Windows and
    // `starts_with` against an app-canonical path would fail. This guard keeps
    // every iac canonicalize site on `dunce`, which is the same passthrough on
    // Unix, where no fixture can show the prefix.
    let source = include_str!("lib.rs");
    let std_calls = source
        .matches(concat!("std::fs::", "canonicalize("))
        .count();
    assert_eq!(std_calls, 0, "iac must canonicalize through dunce");
    assert!(source.matches("dunce::canonicalize(").count() >= 4);
}

#[test]
fn extraction_through_a_symlinked_root_still_expands_local_modules() {
    // AC-0223 (#341): roots, module ancestors and module sources all resolve
    // through one canonicalization, so a root reached through a symlink
    // expands local modules exactly like its canonical path.
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir_all(real.join("modules/queue")).unwrap();
    std::fs::write(
        real.join("main.tf"),
        r#"module "orders" { source = "./modules/queue" }"#,
    )
    .unwrap();
    std::fs::write(
        real.join("modules/queue/main.tf"),
        r#"resource "aws_sqs_queue" "q" {}"#,
    )
    .unwrap();
    let link = dir.path().join("link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&real, &link).unwrap();

    let via_link = extract_dir(&link, &id()).unwrap();
    let direct = extract_dir(&real, &id()).unwrap();
    let ids = |ex: &Extraction| {
        ex.nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<BTreeSet<_>>()
    };
    assert!(ids(&via_link).contains("res:qwtm/infra@module.orders.aws_sqs_queue.q"));
    assert_eq!(ids(&via_link), ids(&direct));
    assert_eq!(terraform_file_count(&link).unwrap(), 2);
}

fn collision_fixture() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tf-address-collision")
}

fn node<'a>(ex: &'a Extraction, id: &str) -> &'a Node {
    ex.nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("missing node {id}"))
}

#[test]
fn same_address_in_two_root_modules_keeps_both_declarations() {
    // AC-1500 (T-1500, #1844, ADR-0077): ids are scoped by root module, so
    // envs/prod and envs/staging both declaring aws_sqs_queue.orders are two
    // nodes, each with its own span and settings. No collision Gap remains.
    let root = collision_fixture();
    let ex = extract_dir(&root, &id()).unwrap();
    for env in ["prod", "staging"] {
        let node = node(
            &ex,
            &format!("res:qwtm/infra@envs/{env}/aws_sqs_queue.orders"),
        );
        assert_eq!(node.props["logical_id"], "aws_sqs_queue.orders");
        assert_eq!(node.props["root_module"], format!("envs/{env}"));
        assert!(node.props.get("child_module").is_none());
        let prov: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
        assert_eq!(prov.evidence[0].path, format!("envs/{env}/main.tf"));
        let source = std::fs::read_to_string(root.join(&prov.evidence[0].path)).unwrap();
        let span =
            &source[prov.evidence[0].byte_start as usize..prov.evidence[0].byte_end as usize];
        assert!(span.contains(&format!("orders-{env}")));
    }
    assert!(
        !ex.nodes
            .iter()
            .any(|node| node.id == "res:qwtm/infra@aws_sqs_queue.orders")
    );
    assert!(
        !ex.nodes
            .iter()
            .any(|node| node.id.starts_with("gap:tf-address-collision:"))
    );

    // Each root module's module call is its own node and instantiates the
    // child's resources under that root module's scope.
    for env in ["prod", "staging"] {
        let module = format!("res:qwtm/infra@envs/{env}/module.{env}_dlq");
        let child = format!("{module}.aws_sqs_queue.dlq");
        assert_eq!(
            node(&ex, &child).props["root_module"],
            format!("envs/{env}")
        );
        assert!(ex.edges.iter().any(|edge| {
            edge.src == module && edge.dst == child && edge.props["relation"] == "MODULE_CONTAINS"
        }));
    }
}

#[test]
fn child_module_reads_are_scoped_by_their_own_directory() {
    // AC-1500: the walk also reads child-module directories directly; those
    // nodes are scoped by the child's directory, carry `child_module` and no
    // `root_module`, and cannot collide with each other or with a root.
    let ex = extract_dir(&collision_fixture(), &id()).unwrap();
    for env in ["prod", "staging"] {
        let direct = node(
            &ex,
            &format!("res:qwtm/infra@modules/{env}_dlq/aws_sqs_queue.dlq"),
        );
        assert_eq!(direct.props["child_module"], format!("modules/{env}_dlq"));
        assert!(direct.props.get("root_module").is_none());
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("modules/queue")).unwrap();
    std::fs::write(
        dir.path().join("main.tf"),
        r#"resource "aws_sqs_queue" "q" {}
module "orders" { source = "./modules/queue" }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("modules/queue/main.tf"),
        r#"resource "aws_sqs_queue" "q" {}"#,
    )
    .unwrap();
    let ex = extract_dir(dir.path(), &id()).unwrap();
    let root_queue = node(&ex, "res:qwtm/infra@aws_sqs_queue.q");
    assert!(root_queue.props.get("root_module").is_none());
    assert!(root_queue.props.get("child_module").is_none());
    assert!(
        node(&ex, "res:qwtm/infra@module.orders.aws_sqs_queue.q")
            .props
            .get("root_module")
            .is_none()
    );
    assert_eq!(
        node(&ex, "res:qwtm/infra@modules/queue/aws_sqs_queue.q").props["child_module"],
        "modules/queue"
    );
    assert!(!ex.nodes.iter().any(|node| node.label == "Gap"));
}

#[test]
fn repository_root_terraform_keeps_unscoped_ids() {
    // AC-1500: Terraform only at the repository root keeps
    // `res:{repo}@{address}` ids and gains no scope prop, so its graph is
    // unchanged by root-module scoping.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("main.tf"),
        r#"resource "aws_sqs_queue" "orders" {}
resource "aws_lambda_function" "fulfill" { depends_on = [aws_sqs_queue.orders] }
"#,
    )
    .unwrap();
    let ex = extract_dir(dir.path(), &id()).unwrap();
    let ids: BTreeSet<_> = ex.nodes.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(
        ids,
        BTreeSet::from([
            "res:qwtm/infra@aws_lambda_function.fulfill",
            "res:qwtm/infra@aws_sqs_queue.orders",
        ])
    );
    for node in &ex.nodes {
        assert!(node.props.get("root_module").is_none());
        assert!(node.props.get("child_module").is_none());
    }
}

#[test]
fn edges_and_chased_grants_stay_inside_their_root_module() {
    // AC-1500: every edge target takes the scope of the block that names it,
    // including GRANTS rebuilt from a policy document and placeholders.
    let dir = tempfile::tempdir().unwrap();
    for env in ["prod", "staging"] {
        std::fs::create_dir_all(dir.path().join(format!("envs/{env}"))).unwrap();
        std::fs::write(
            dir.path().join(format!("envs/{env}/main.tf")),
            r#"
resource "aws_sqs_queue" "orders" {}
resource "aws_lambda_function" "fulfill" { depends_on = [aws_sqs_queue.orders] }
resource "aws_lambda_event_source_mapping" "orders" {
  event_source_arn = aws_sqs_queue.orders.arn
  function_name    = aws_lambda_function.fulfill.arn
}
data "aws_iam_policy_document" "orders" {
  statement {
    actions   = ["sqs:SendMessage"]
    resources = [aws_sqs_queue.orders.arn]
  }
}
resource "aws_iam_policy" "orders" {
  policy = data.aws_iam_policy_document.orders.json
}
resource "aws_sns_topic" "alerts" { kms_master_key_id = aws_kms_key.missing.arn }
"#,
        )
        .unwrap();
    }
    let ex = extract_dir(dir.path(), &id()).unwrap();
    for env in ["prod", "staging"] {
        let scoped = |address: &str| format!("res:qwtm/infra@envs/{env}/{address}");
        let has = |src: &str, dst: &str, label: &str| {
            ex.edges.iter().any(|edge| {
                edge.src == scoped(src) && edge.dst == scoped(dst) && edge.label == label
            })
        };
        assert!(has(
            "aws_lambda_function.fulfill",
            "aws_sqs_queue.orders",
            "DEPENDS_ON"
        ));
        assert!(has(
            "aws_sqs_queue.orders",
            "aws_lambda_function.fulfill",
            "TRIGGERS"
        ));
        assert!(has(
            "aws_iam_policy.orders",
            "aws_sqs_queue.orders",
            "GRANTS"
        ));
        let placeholder = node(&ex, &scoped("aws_kms_key.missing"));
        assert_eq!(placeholder.props["root_module"], format!("envs/{env}"));
    }
    for edge in &ex.edges {
        let src_scope = terraform_id_scope(&edge.src).unwrap().1;
        let dst_scope = terraform_id_scope(&edge.dst).unwrap().1;
        assert_eq!(src_scope, dst_scope, "{} -> {}", edge.src, edge.dst);
    }
}

#[test]
fn every_root_module_directory_is_reported_even_without_resources() {
    // AC-1501: a root module that declares no resource is still a root
    // module, so observed state is not tied to the other one by default;
    // child-module directories and the repository root are not listed.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("envs/prod")).unwrap();
    std::fs::create_dir_all(dir.path().join("envs/staging")).unwrap();
    std::fs::create_dir_all(dir.path().join("modules/q")).unwrap();
    std::fs::write(
        dir.path().join("main.tf"),
        r#"resource "aws_sqs_queue" "r" {}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("envs/prod/main.tf"),
        r#"module "q" { source = "../../modules/q" }"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("envs/staging/variables.tf"),
        r#"variable "region" {}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("modules/q/main.tf"),
        r#"resource "aws_sqs_queue" "q" {}"#,
    )
    .unwrap();
    let ex = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        ex.root_modules,
        BTreeSet::from(["envs/prod".to_string(), "envs/staging".to_string()])
    );
}

#[test]
fn resource_id_from_node_keeps_the_source_scope() {
    // AC-1500: the policy-document chase rebuilds a target from its source's
    // id and must keep that source's root-module scope.
    assert_eq!(
        resource_id_from_node(
            "res:qwtm/infra@envs/prod/aws_iam_policy.p",
            "aws_sqs_queue.q"
        ),
        "res:qwtm/infra@envs/prod/aws_sqs_queue.q"
    );
    assert_eq!(
        resource_id_from_node("res:qwtm/infra@aws_iam_policy.p", "aws_sqs_queue.q"),
        "res:qwtm/infra@aws_sqs_queue.q"
    );
}

#[test]
fn root_module_scoping_is_deterministic() {
    // AC-1500: cold, repeated and warm-cache runs produce identical facts.
    let root = collision_fixture();
    let a = extract_dir(&root, &id()).unwrap();
    let b = extract_dir(&root, &id()).unwrap();
    assert_eq!(
        serde_json::to_string(&a.nodes).unwrap(),
        serde_json::to_string(&b.nodes).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&a.edges).unwrap(),
        serde_json::to_string(&b.edges).unwrap()
    );
    let mut cache = IncrementalCache::default();
    let _ = extract_dir_incremental(&root, &id(), &mut cache).unwrap();
    let (warm, stats) = extract_dir_incremental(&root, &id(), &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 0);
    assert_eq!(
        serde_json::to_string(&warm.nodes).unwrap(),
        serde_json::to_string(&a.nodes).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&warm.edges).unwrap(),
        serde_json::to_string(&a.edges).unwrap()
    );
}

// AC-1053: a captured source's `.tf` files are parsed from the bytes the
// host supplies, never the checkout; a local module file outside them is
// still read, from the checkout, so the module expands as before.
#[test]
fn supplied_terraform_bytes_are_parsed_instead_of_the_checkout() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("modules/queue")).unwrap();
    std::fs::write(
        dir.path().join("main.tf"),
        "resource \"aws_sqs_queue\" \"checkout\" {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("modules/queue/queue.tf"),
        "resource \"aws_sqs_queue\" \"inner\" {}\n",
    )
    .unwrap();
    let paths = tf_paths(dir.path()).unwrap();
    assert_eq!(paths, ["main.tf", "modules/queue/queue.tf"]);
    let captured = "module \"queue\" {\n  source = \"./modules/queue\"\n}\nresource \"aws_sqs_queue\" \"captured\" {}\n";
    let (extraction, stats) = extract_dir_from_sources(
        dir.path(),
        &id(),
        &paths[..1],
        &|path| {
            assert_eq!(path, "main.tf");
            Ok(captured.as_bytes().to_vec())
        },
        &mut |_| {},
    )
    .unwrap();
    let ids: Vec<&str> = extraction
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect();
    assert!(
        ids.iter().any(|id| id.ends_with("aws_sqs_queue.captured")),
        "{ids:?}"
    );
    assert!(!ids.iter().any(|id| id.ends_with("aws_sqs_queue.checkout")));
    assert!(
        ids.iter()
            .any(|id| id.ends_with("module.queue.aws_sqs_queue.inner")),
        "{ids:?}"
    );
    assert_eq!(stats.distinct_files, 2);
}

/// Walk `files` of `root` from their checkout bytes as if captured,
/// reporting what else the walk read.
fn reads_of(root: &Path, files: &[&str]) -> SourceReads {
    let paths: Vec<String> = files.iter().map(|path| path.to_string()).collect();
    let root_dir = root.to_path_buf();
    extract_dir_from_sources_reading(
        root,
        &id(),
        &paths,
        &|path| std::fs::read(root_dir.join(path)),
        &mut |_| {},
    )
    .unwrap()
    .2
}

// AC-1526: a walk over supplied sources reports what it read besides them:
// a module file they do not hold, each local module directory listed, a
// remote module source and an unresolved local one. It is bound to them
// only when it read nothing else and each listed directory holds them alone.
#[test]
fn a_walk_over_supplied_sources_reports_what_else_it_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("modules/queue")).unwrap();
    std::fs::write(
        root.join("main.tf"),
        "module \"queue\" {\n  source = \"./modules/queue\"\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("modules/queue/queue.tf"),
        "resource \"aws_sqs_queue\" \"inner\" {}\n",
    )
    .unwrap();
    let all = ["main.tf", "modules/queue/queue.tf"];
    let paths: Vec<String> = all.iter().map(|path| path.to_string()).collect();
    let bound = reads_of(root, &all);
    assert!(bound.outside.is_empty());
    assert_eq!(
        bound.listed.get("modules/queue").map(Vec::as_slice),
        Some(&paths[1..])
    );
    assert!(!bound.remote_module && !bound.unresolved_module);
    assert!(bound.bound_to(&paths));
    // The module file is not among the supplied sources: read from the
    // checkout, so the walk is not bound to them.
    let partial = reads_of(root, &all[..1]);
    assert!(partial.outside.contains("modules/queue/queue.tf"));
    assert!(!partial.bound_to(&paths[..1]));
    // A listed directory holding a file the sources lack is not bound.
    assert!(!bound.bound_to(&paths[..1]));
    std::fs::write(
        root.join("main.tf"),
        "module \"vpc\" {\n  source = \"terraform-aws-modules/vpc/aws\"\n}\n\
         module \"gone\" {\n  source = \"./modules/gone\"\n}\n",
    )
    .unwrap();
    let flagged = reads_of(root, &all);
    assert!(flagged.remote_module);
    assert!(flagged.unresolved_module);
    assert!(!flagged.bound_to(&paths));
}
