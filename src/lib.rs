use chrono::{NaiveDate, TimeZone, Utc};
use futures::future;
use log::{info, warn};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json;
use std::time::Duration;

#[derive(Debug, Deserialize, Clone)]
pub struct Commit {
    pub sha: String,
    pub commit: CommitDetail,
    pub html_url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CommitDetail {
    pub message: String,
    pub author: CommitAuthor,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CommitAuthor {
    pub name: String,
    pub email: String,
    pub date: String,
}

#[derive(Debug, Deserialize)]
struct CommitDetailResponse {
    #[allow(dead_code)]
    sha: String,
    #[allow(dead_code)]
    commit: CommitDetail,
    #[allow(dead_code)]
    html_url: String,
}

#[derive(Debug, Deserialize)]
pub struct RepoInfo {
    pub default_branch: String,
}

#[derive(Debug, Serialize)]
pub struct CommitInfo {
    pub sha: String,
    pub message: String,
    pub author: String,
    pub email: String,
    pub date: String,
    pub html_url: String,
}

pub struct GitHubClient {
    client: Client,
    token: Option<String>,
}

impl GitHubClient {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("github-commit-info")
            .build()
            .expect("Failed to create HTTP client");
        
        let token = std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty());
        
        Self { client, token }
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            "application/vnd.github.v3+json".parse().unwrap(),
        );
        if let Some(token) = &self.token {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", token).parse().unwrap(),
            );
        }
        headers
    }

    pub async fn get_default_branch(&self, owner: &str, repo: &str) -> Result<String, String> {
        let url = format!("https://api.github.com/repos/{}/{}", owner, repo);

        let response = self
            .client
            .get(&url)
            .headers(self.headers())
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !response.status().is_success() {
            let status = response.status();
            if status.as_u16() == 403 {
                return Err("API请求被拒绝(403)。请设置 GITHUB_TOKEN 环境变量".to_string());
            }
            return Err(format!("获取仓库信息失败: {}", status));
        }

        let repo_info: RepoInfo = response.json().await.map_err(|e| e.to_string())?;
        Ok(repo_info.default_branch)
    }

    pub fn get_repo_from_url(&self, url: &str) -> Option<(String, String)> {
        let url = url.trim_end_matches('/');

        if url.contains("github.com") {
            let parts: Vec<&str> = url.split('/').collect();
            if parts.len() >= 2 {
                let owner = parts[parts.len() - 2];
                let repo = parts[parts.len() - 1].trim_end_matches(".git");
                return Some((owner.to_string(), repo.to_string()));
            }
        }
        None
    }

    pub async fn fetch_commits(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        since: &str,
        until: &str,
    ) -> Result<Vec<Commit>, String> {
        let url = format!("https://api.github.com/repos/{}/{}/commits", owner, repo);

        let response = self
            .client
            .get(&url)
            .headers(self.headers())
            .query(&[
                ("sha", branch),
                ("since", since),
                ("until", until),
                ("per_page", "100"),
            ])
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !response.status().is_success() {
            let status = response.status();
            if status.as_u16() == 403 {
                return Err("API请求被拒绝(403)。请设置 GITHUB_TOKEN 环境变量".to_string());
            }
            return Err(format!("API请求失败: {}", status));
        }

        let commits: Vec<Commit> = response.json().await.map_err(|e| e.to_string())?;
        Ok(commits)
    }

    async fn fetch_commit_detail(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
    ) -> Result<CommitDetailResponse, String> {
        if owner.is_empty() || repo.is_empty() {
            return Err("owner and repo are required".to_string());
        }
        let url = format!("https://api.github.com/repos/{}/{}/commits/{}", owner, repo, sha);

        let response = self
            .client
            .get(&url)
            .headers(self.headers())
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !response.status().is_success() {
            return Err(format!("获取提交详情失败: {}", response.status()));
        }

        let detail: CommitDetailResponse = response.json().await.map_err(|e| e.to_string())?;
        Ok(detail)
    }

    pub async fn get_commit_info(
        &self,
        owner: &str,
        repo: &str,
        commit: Commit,
    ) -> Result<CommitInfo, String> {
        let _detail = self.fetch_commit_detail(owner, repo, &commit.sha).await?;

        Ok(CommitInfo {
            sha: commit.sha,
            message: commit.commit.message,
            author: commit.commit.author.name,
            email: commit.commit.author.email,
            date: commit.commit.author.date,
            html_url: commit.html_url,
        })
    }
}

impl Default for GitHubClient {
    fn default() -> Self {
        Self::new()
    }
}

pub fn parse_start_date(date_str: &str) -> Result<String, String> {
    let naive_date =
        NaiveDate::parse_from_str(date_str, "%Y-%m-%d").map_err(|_| {
            format!("日期格式错误: {}, 期望格式: yyyy-MM-dd", date_str)
        })?;

    let datetime = naive_date.and_hms_opt(0, 0, 0).ok_or("日期解析失败")?;

    let utc_datetime = Utc.from_utc_datetime(&datetime);
    Ok(utc_datetime.to_rfc3339())
}

pub fn calculate_until_date(start_date: &str, days: i64) -> Result<String, String> {
    let naive_date =
        NaiveDate::parse_from_str(start_date, "%Y-%m-%d").map_err(|_| {
            format!("日期格式错误: {}", start_date)
        })?;

    let end_date = naive_date + chrono::Duration::days(days);

    let datetime = end_date.and_hms_opt(0, 0, 0).ok_or("日期解析失败")?;

    let utc_datetime = Utc.from_utc_datetime(&datetime);
    Ok(utc_datetime.to_rfc3339())
}

#[tokio::main]
pub async fn run(
    url: &str,
    branch: Option<&str>,
    start_date: &str,
    days: i64,
    output: Option<&str>,
) -> Result<(), String> {
    let client = GitHubClient::new();

    let (owner, repo) = client
        .get_repo_from_url(url)
        .ok_or_else(|| "无法从URL解析owner/repo".to_string())?;

    let branch = if let Some(b) = branch {
        b.to_string()
    } else {
        info!("正在获取仓库默认分支...");
        client.get_default_branch(&owner, &repo).await?
    };

    let since = parse_start_date(start_date)?;
    let until = calculate_until_date(start_date, days)?;

    info!("仓库: {}/{}", owner, repo);
    info!("分支: {}", branch);
    info!(
        "时间范围: {} 至 {} (共{}天)",
        start_date,
        until.split('T').next().unwrap_or(&until),
        days
    );

    let commits = client
        .fetch_commits(&owner, &repo, &branch, &since, &until)
        .await?;

    if commits.is_empty() {
        info!("在指定时间范围内没有找到提交");
        return Ok(());
    }

    info!("找到 {} 个提交", commits.len());
    info!("正在并发获取提交详情...");

    let futures = commits.into_iter().map(|commit| {
        client.get_commit_info(&owner, &repo, commit)
    });
    let results = future::join_all(futures).await;

    let mut success_count = 0;
    let mut commit_infos = Vec::new();
    for result in results {
        match result {
            Ok(info) => {
                success_count += 1;
                commit_infos.push(info);
            }
            Err(e) => warn!("获取提交详情失败: {}", e),
        }
    }
    info!("成功获取 {} 个提交详情", success_count);

    let json = serde_json::to_string_pretty(&commit_infos).map_err(|e| e.to_string())?;

    if let Some(path) = output {
        tokio::fs::write(path, &json).await.map_err(|e| e.to_string())?;
        info!("结果已保存到 {}", path);
    } else {
        println!("{}", json);
    }

    Ok(())
}
