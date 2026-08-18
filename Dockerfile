# i-agent：轻量办公主力 agent（静态 musl 构建，最终镜像仅约 15MB）
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY assets ./assets
RUN cargo build --release && \
    cp target/release/i-agent /i-agent && \
    strip /i-agent || true

FROM alpine:3.20
RUN apk add --no-cache ca-certificates
COPY --from=build /i-agent /usr/local/bin/i-agent
# 技能包已编译进二进制，首次运行自动释放；也可挂载覆盖 /root/.i-agent/assets
WORKDIR /work
ENTRYPOINT ["i-agent"]
# 用法:
#   docker build -t i-agent .
#   docker run --rm -e MINIMAX_API_KEY=xxx -v %cd%:/work i-agent -p "任务"
