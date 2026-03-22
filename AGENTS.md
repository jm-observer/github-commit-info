# Dependencies

```
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
log = "0.4"
custom-utils = "0.10.21"
tokio = { version = "1", features = ["full"] }
```

- `reqwest`: HTTP 客户端，使用 rustls-tls（不使用 openssl）
- `log`: 日志记录
- `custom-utils`: 自定义工具库（logger）
- `tokio`: 异步运行时
