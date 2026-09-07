use settings::{RegisterSetting, Settings};

use crate::host_bitbucket::BitbucketCredentials;

#[derive(Clone, Debug, Default, PartialEq, RegisterSetting)]
pub(crate) struct PullRequestReviewSettings {
    pub(crate) bitbucket: Option<BitbucketCredentials>,
}

impl Settings for PullRequestReviewSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let bitbucket = content
            .pull_request_review
            .as_ref()
            .and_then(|settings| settings.bitbucket.as_ref())
            .and_then(|bitbucket| {
                BitbucketCredentials::from_settings(
                    bitbucket.access_token.clone(),
                    bitbucket.username.clone(),
                    bitbucket.app_password.clone(),
                )
            });

        Self { bitbucket }
    }
}
