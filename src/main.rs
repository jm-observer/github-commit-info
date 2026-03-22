use clap::Parser;
use github_commit_info::run;
use log::LevelFilter::Info;

#[derive(Parser, Debug)]
#[command(
    name = "github-commit-info",
    about = "获取GitHub仓库指定时间范围内的commit信息",
    long_about = None
)]
struct Args {
    #[arg(long, help = "GitHub仓库URL (如: https://github.com/golang/go)")]
    url: String,

    #[arg(long, help = "分支名称 (如不指定则自动获取默认分支)")]
    branch: Option<String>,

    #[arg(long, help = "起始日期, 格式: yyyy-MM-dd (默认昨天)")]
    start_date: Option<String>,

    #[arg(long, help = "从起始日期开始的天数 (默认1)")]
    days: Option<i64>,

    #[arg(long, help = "输出文件路径 (默认为stdout)")]
    output: Option<String>,
}

fn main() {
    let _ = custom_utils::logger::logger_feature(
        "github-commit-info",
        "debug,reqwest=info",
        Info,
        false,
    )
    .build();

    let _ = std::env::var("GITHUB_TOKEN").expect("请设置 GITHUB_TOKEN 环境变量");

    let args = Args::parse();

    let start_date = args.start_date.unwrap_or_else(|| {
        let yesterday = chrono::Utc::now() - chrono::Duration::days(1);
        yesterday.format("%Y-%m-%d").to_string()
    });

    let days = args.days.unwrap_or(1);

    if let Err(e) = run(
        &args.url,
        args.branch.as_deref(),
        &start_date,
        days,
        args.output.as_deref(),
    ) {
        eprintln!("错误: {}", e);
        std::process::exit(1);
    }
}
