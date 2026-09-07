//! Bitbucket pull request host implementation using the Bitbucket REST API.

use std::sync::Arc;

use ::settings::Settings as _;
use base64::Engine as _;
use chrono::Utc;
use futures::AsyncReadExt as _;
use gpui::{App, AppContext as _, Task};
use http_client::{AsyncBody, HttpClient, HttpRequestExt, Method, Request, Response, StatusCode};
use serde_json::Value;
use url::Url;

use crate::changeset::ChangedFile;
use crate::host::{
    CommentThread, DiffSide, DraftComment, HostError, Identity, ListQuery, PullRequestDetail,
    PullRequestHost, PullRequestId, PullRequestSummary, RepositoryCoordinates, SortDirection,
    StateFilter,
};
use crate::host_twg::{
    parse_comment, parse_comments, parse_detail, parse_diffstat, parse_identity, parse_list,
};
use crate::settings::PullRequestReviewSettings;

const DIFFSTAT_LIMIT: usize = 2000;
const COMMENT_LIMIT: usize = 1000;
pub const DEFAULT_LIST_LIMIT: usize = 100;
const ALL_STATES: [&str; 4] = ["OPEN", "MERGED", "DECLINED", "SUPERSEDED"];
const BITBUCKET_API_BASE_URL: &str = "https://api.bitbucket.org/2.0";
pub fn supports_remote_host(host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    host == "bitbucket.org" || host == "api.bitbucket.org"
}

pub struct BitbucketApiHost {
    client: Arc<dyn HttpClient>,
    credentials: Option<BitbucketCredentials>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BitbucketCredentials {
    Bearer(String),
    Basic { username: String, password: String },
}

impl BitbucketApiHost {
    pub fn new(cx: &mut App) -> Self {
        Self {
            client: cx.http_client(),
            credentials: PullRequestReviewSettings::get_global(cx).bitbucket.clone(),
        }
    }
}

impl PullRequestHost for BitbucketApiHost {
    fn list(
        &self,
        repository: &RepositoryCoordinates,
        query: ListQuery,
        cx: &App,
    ) -> Task<Result<Vec<PullRequestSummary>, HostError>> {
        let client = self.client.clone();
        let credentials = self.credentials.clone();
        let repository = repository.clone();

        cx.spawn(async move |cx| {
            let mut url = pull_requests_url(&repository)?;
            {
                let mut pairs = url.query_pairs_mut();
                match query.state {
                    StateFilter::OpenAndDraft => {
                        pairs.append_pair("state", "OPEN");
                    }
                    StateFilter::All => {
                        for state in ALL_STATES {
                            pairs.append_pair("state", state);
                        }
                    }
                }
                if let Some(author) = author_filter_nickname(query.author.as_ref()) {
                    pairs.append_pair("q", &format!(r#"author.nickname="{author}""#));
                }
                pairs.append_pair("pagelen", &query.limit.max(1).min(100).to_string());
                pairs.append_pair("sort", sort_query(query.sort));
            }
            let payload =
                get_paginated_json(client, credentials, url, query.limit.max(1), cx).await?;
            let summaries = parse_list(&payload, &repository, Utc::now())?;
            Ok(crate::host_twg::order_and_limit(
                summaries,
                query.sort,
                query.limit,
            ))
        })
    }

    fn detail(&self, id: &PullRequestId, cx: &App) -> Task<Result<PullRequestDetail, HostError>> {
        let client = self.client.clone();
        let credentials = self.credentials.clone();
        let id = id.clone();
        cx.spawn(async move |cx| {
            let url = pull_request_url(&id)?;
            let payload = get_json(client, credentials, url, cx).await?;
            parse_detail(&payload, &id.repository, Utc::now())
        })
    }

    fn changed_files(
        &self,
        id: &PullRequestId,
        cx: &App,
    ) -> Task<Result<Vec<ChangedFile>, HostError>> {
        let client = self.client.clone();
        let credentials = self.credentials.clone();
        let id = id.clone();
        cx.spawn(async move |cx| {
            let mut url = api_url(&format!(
                "/repositories/{}/{}/pullrequests/{}/diffstat",
                encode_path(&id.repository.owner),
                encode_path(&id.repository.name),
                id.number
            ))?;
            url.query_pairs_mut()
                .append_pair("pagelen", &DIFFSTAT_LIMIT.min(100).to_string());
            let payload = get_paginated_json(client, credentials, url, DIFFSTAT_LIMIT, cx).await?;
            parse_diffstat(&payload)
        })
    }

    fn comments(
        &self,
        id: &PullRequestId,
        cx: &App,
    ) -> Task<Result<Vec<CommentThread>, HostError>> {
        let client = self.client.clone();
        let credentials = self.credentials.clone();
        let id = id.clone();
        cx.spawn(async move |cx| {
            let mut url = api_url(&format!(
                "/repositories/{}/{}/pullrequests/{}/comments",
                encode_path(&id.repository.owner),
                encode_path(&id.repository.name),
                id.number
            ))?;
            url.query_pairs_mut()
                .append_pair("pagelen", &COMMENT_LIMIT.min(100).to_string());
            let payload = get_paginated_json(client, credentials, url, COMMENT_LIMIT, cx).await?;
            parse_comments(&payload)
        })
    }

    fn post_comment(
        &self,
        id: &PullRequestId,
        draft: DraftComment,
        cx: &App,
    ) -> Task<Result<CommentThread, HostError>> {
        let client = self.client.clone();
        let credentials = self.credentials.clone();
        let id = id.clone();
        cx.spawn(async move |cx| {
            let url = api_url(&format!(
                "/repositories/{}/{}/pullrequests/{}/comments",
                encode_path(&id.repository.owner),
                encode_path(&id.repository.name),
                id.number
            ))?;
            let body = draft_comment_body(&draft);
            let payload = send_json(client, credentials, Method::POST, url, Some(body), cx).await?;
            let value = parse_json_value(&payload)?;
            parse_comment(&value).ok_or_else(|| HostError::UnexpectedResponse {
                detail: "the posted comment was not echoed back in a recognisable shape".into(),
                version: None,
            })
        })
    }

    fn viewer(&self, cx: &App) -> Task<Result<Identity, HostError>> {
        let client = self.client.clone();
        let credentials = self.credentials.clone();
        cx.spawn(async move |cx| {
            let payload = get_json(client, credentials, api_url("/user")?, cx).await?;
            let value = parse_json_value(&payload)?;
            parse_identity(&value).ok_or_else(|| HostError::UnexpectedResponse {
                detail: "the signed-in account was not reported in a recognisable shape".into(),
                version: None,
            })
        })
    }
}

impl BitbucketCredentials {
    pub fn from_settings(
        access_token: Option<String>,
        username: Option<String>,
        app_password: Option<String>,
    ) -> Option<Self> {
        let access_token = non_empty(access_token);
        if let Some(token) = access_token {
            return Some(Self::Bearer(token));
        }

        Some(Self::Basic {
            username: non_empty(username)?,
            password: non_empty(app_password)?,
        })
    }

    fn authorization_header(&self) -> String {
        match self {
            Self::Bearer(token) => format!("Bearer {token}"),
            Self::Basic { username, password } => {
                let encoded = base64::engine::general_purpose::STANDARD
                    .encode(format!("{username}:{password}"));
                format!("Basic {encoded}")
            }
        }
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn get_json(
    client: Arc<dyn HttpClient>,
    credentials: Option<BitbucketCredentials>,
    url: Url,
    cx: &mut gpui::AsyncApp,
) -> Result<String, HostError> {
    send_json(client, credentials, Method::GET, url, None, cx).await
}

async fn get_paginated_json(
    client: Arc<dyn HttpClient>,
    credentials: Option<BitbucketCredentials>,
    first_url: Url,
    limit: usize,
    cx: &mut gpui::AsyncApp,
) -> Result<String, HostError> {
    let mut next_url = Some(first_url);
    let mut values = Vec::new();

    while let Some(url) = next_url.take() {
        let page = get_json(client.clone(), credentials.clone(), url, cx).await?;
        let page = parse_json_value(&page)?;
        let Some(page_values) = page.get("values").and_then(Value::as_array) else {
            return Ok(page.to_string());
        };

        values.extend(
            page_values
                .iter()
                .take(limit.saturating_sub(values.len()))
                .cloned(),
        );
        if values.len() >= limit {
            break;
        }

        next_url = page
            .get("next")
            .and_then(Value::as_str)
            .and_then(|url| Url::parse(url).ok());
    }

    Ok(serde_json::json!({ "values": values }).to_string())
}

async fn send_json(
    client: Arc<dyn HttpClient>,
    credentials: Option<BitbucketCredentials>,
    method: Method,
    url: Url,
    body: Option<Value>,
    cx: &mut gpui::AsyncApp,
) -> Result<String, HostError> {
    let response = request(client, credentials, method, url, body).await?;
    cx.background_spawn(async move { response_body(response).await })
        .await
}

async fn request(
    client: Arc<dyn HttpClient>,
    credentials: Option<BitbucketCredentials>,
    method: Method,
    url: Url,
    body: Option<Value>,
) -> Result<Response<AsyncBody>, HostError> {
    let mut builder = Request::builder()
        .method(method)
        .uri(url.as_str())
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .follow_redirects(http_client::RedirectPolicy::FollowAll);
    if let Some(credentials) = credentials {
        builder = builder.header("Authorization", credentials.authorization_header());
    }
    let body = match body {
        Some(body) => AsyncBody::from(serde_json::to_vec(&body).map_err(|error| {
            HostError::UnexpectedResponse {
                detail: format!("the comment body could not be encoded: {error}"),
                version: None,
            }
        })?),
        None => AsyncBody::empty(),
    };
    let response = client
        .send(
            builder
                .body(body)
                .map_err(|error| HostError::UnexpectedResponse {
                    detail: format!("the request could not be built: {error}"),
                    version: None,
                })?,
        )
        .await
        .map_err(|error| HostError::Unreachable {
            detail: error.to_string(),
        })?;
    classify_response(response)
}

fn classify_response(response: Response<AsyncBody>) -> Result<Response<AsyncBody>, HostError> {
    match response.status() {
        StatusCode::OK | StatusCode::CREATED => Ok(response),
        StatusCode::UNAUTHORIZED => Err(HostError::NotAuthenticated),
        StatusCode::FORBIDDEN => Err(HostError::PermissionDenied {
            repository: "this repository".into(),
        }),
        StatusCode::NOT_FOUND => Err(HostError::RepositoryNotFound {
            repository: "this repository".into(),
        }),
        StatusCode::TOO_MANY_REQUESTS => Err(HostError::RateLimited),
        status if status.is_server_error() => Err(HostError::Unreachable {
            detail: format!("the Bitbucket API returned HTTP {status}"),
        }),
        status => Err(HostError::UnexpectedResponse {
            detail: format!("the Bitbucket API returned HTTP {status}"),
            version: None,
        }),
    }
}

async fn response_body(mut response: Response<AsyncBody>) -> Result<String, HostError> {
    let mut body = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut body)
        .await
        .map_err(|error| HostError::Unreachable {
            detail: error.to_string(),
        })?;
    String::from_utf8(body).map_err(|error| HostError::UnexpectedResponse {
        detail: format!("the Bitbucket API returned non-UTF-8 output: {error}"),
        version: None,
    })
}

fn parse_json_value(payload: &str) -> Result<Value, HostError> {
    serde_json::from_str(payload).map_err(|error| HostError::UnexpectedResponse {
        detail: format!("unreadable output: {error}"),
        version: None,
    })
}

fn api_url(path: &str) -> Result<Url, HostError> {
    Url::parse(&format!("{BITBUCKET_API_BASE_URL}{path}")).map_err(|error| {
        HostError::UnexpectedResponse {
            detail: format!("the Bitbucket API URL could not be built: {error}"),
            version: None,
        }
    })
}

fn pull_requests_url(repository: &RepositoryCoordinates) -> Result<Url, HostError> {
    api_url(&format!(
        "/repositories/{}/{}/pullrequests",
        encode_path(&repository.owner),
        encode_path(&repository.name)
    ))
}

fn pull_request_url(id: &PullRequestId) -> Result<Url, HostError> {
    api_url(&format!(
        "/repositories/{}/{}/pullrequests/{}",
        encode_path(&id.repository.owner),
        encode_path(&id.repository.name),
        id.number
    ))
}

fn encode_path(segment: &str) -> String {
    urlencoding::encode(segment).into_owned()
}

fn sort_query(sort: SortDirection) -> &'static str {
    match sort {
        SortDirection::MostRecentFirst => "-updated_on",
        SortDirection::LeastRecentFirst => "updated_on",
    }
}

fn author_filter_nickname(filter: Option<&crate::host::AuthorFilter>) -> Option<String> {
    match filter {
        Some(crate::host::AuthorFilter::Person { nickname, .. }) if !nickname.is_empty() => {
            Some(nickname.clone())
        }
        _ => None,
    }
}

fn draft_comment_body(draft: &DraftComment) -> Value {
    let mut inline = serde_json::json!({
        "path": draft.path.as_unix_str().to_string(),
    });
    match draft.side {
        DiffSide::New => inline["to"] = Value::from(draft.line),
        DiffSide::Old => inline["from"] = Value::from(draft.line),
    }

    let mut body = serde_json::json!({
        "content": { "raw": draft.body },
        "inline": inline,
    });
    if let Some(parent) = &draft.reply_to {
        body["parent"] = match parent.0.parse::<u64>() {
            Ok(parent_id) => serde_json::json!({ "id": parent_id }),
            Err(_) => serde_json::json!({ "id": parent.0 }),
        };
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_client::Response;

    fn repository() -> RepositoryCoordinates {
        RepositoryCoordinates {
            owner: "atlassian".into(),
            name: "twg-cli".into(),
        }
    }

    #[test]
    fn only_cloud_bitbucket_hosts_are_supported() {
        assert!(supports_remote_host("bitbucket.org"));
        assert!(supports_remote_host(" api.bitbucket.org "));
        assert!(!supports_remote_host("github.com"));
        assert!(!supports_remote_host("bitbucket.example.com"));
    }

    #[test]
    fn credentials_prefer_bearer_tokens() {
        let mut env: collections::HashMap<String, String> = collections::HashMap::default();
        env.insert("BITBUCKET_ACCESS_TOKEN".into(), "token".into());
        env.insert("BITBUCKET_USERNAME".into(), "user".into());
        env.insert("BITBUCKET_APP_PASSWORD".into(), "password".into());
        let credentials = BitbucketCredentials::from_settings(
            env.get("BITBUCKET_ACCESS_TOKEN").cloned(),
            env.get("BITBUCKET_USERNAME").cloned(),
            env.get("BITBUCKET_APP_PASSWORD").cloned(),
        )
        .expect("credentials");
        assert_eq!(credentials.authorization_header(), "Bearer token");
    }

    #[test]
    fn basic_credentials_use_app_passwords() {
        let mut env: collections::HashMap<String, String> = collections::HashMap::default();
        env.insert("BITBUCKET_USERNAME".into(), "user".into());
        env.insert("BITBUCKET_APP_PASSWORD".into(), "password".into());
        let credentials = BitbucketCredentials::from_settings(
            env.get("BITBUCKET_ACCESS_TOKEN").cloned(),
            env.get("BITBUCKET_USERNAME").cloned(),
            env.get("BITBUCKET_APP_PASSWORD").cloned(),
        )
        .expect("credentials");
        assert_eq!(
            credentials.authorization_header(),
            "Basic dXNlcjpwYXNzd29yZA=="
        );
    }

    #[test]
    fn draft_comment_body_uses_new_or_old_side_line() {
        let draft = DraftComment {
            path: git::repository::RepoPath::new("src/main.rs").expect("valid path"),
            side: DiffSide::New,
            line: 42,
            body: "Please explain this.".into(),
            reply_to: None,
            against_revision: "abc".into(),
        };
        let body = draft_comment_body(&draft);
        assert_eq!(body["inline"]["to"], 42);
        assert!(body["inline"].get("from").is_none());

        let mut old = draft;
        old.side = DiffSide::Old;
        let body = draft_comment_body(&old);
        assert_eq!(body["inline"]["from"], 42);
        assert!(body["inline"].get("to").is_none());
    }

    #[test]
    fn response_statuses_are_classified() {
        let response = |status| {
            Response::builder()
                .status(status)
                .body(AsyncBody::empty())
                .unwrap()
        };
        assert!(classify_response(response(200)).is_ok());
        assert!(matches!(
            classify_response(response(401)),
            Err(HostError::NotAuthenticated)
        ));
        assert!(matches!(
            classify_response(response(403)),
            Err(HostError::PermissionDenied { .. })
        ));
        assert!(matches!(
            classify_response(response(404)),
            Err(HostError::RepositoryNotFound { .. })
        ));
        assert!(matches!(
            classify_response(response(429)),
            Err(HostError::RateLimited)
        ));
    }

    #[test]
    fn pull_request_urls_target_the_bitbucket_rest_api() {
        let id = PullRequestId {
            number: 123,
            repository: repository(),
        };
        assert_eq!(
            pull_request_url(&id).expect("url").as_str(),
            "https://api.bitbucket.org/2.0/repositories/atlassian/twg-cli/pullrequests/123"
        );
    }
}
