_default:
    @just --list

# 运行副屏实时界面，其余参数透传给 msu2-slint：
#   just screen
#   just screen --fps 30
#   just screen --ucas-user 学号 --ucas-password 密码
screen *ARGS:
    cargo run -q -p msu2-slint -- {{ARGS}}
