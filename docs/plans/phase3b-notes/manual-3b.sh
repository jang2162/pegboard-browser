#!/bin/zsh
# manual-3b.sh <명령>: 3b 손 확인 도우미. 임시 홈과 임시 사용자 데이터 디렉터리로 Pegboard.app을
# 켜고, 확인 창과 데몬 포기 알림을 하나씩 띄운다. 사람은 보고 누르기만 한다.
#
#   start       앱을 켠다(임시 프로필, 시험 훅 없음) + 업로드 시험 페이지 서버
#   upload      세션 하나가 파일 업로드를 묻는다(5초 뒤). 버튼 둘(허용, 거부)
#   upload-dir  --upload-dir 세션이 업로드를 묻는다(5초 뒤). 버튼 셋
#   state       세션이 쿠키 쓰기를 묻는다(5초 뒤)
#   storage     세션이 localStorage 쓰기를 묻는다(5초 뒤)
#   giveup      데몬을 1분 안에 네 번 죽여 "데몬이 멈췄습니다" 알림을 띄운다
#   status      앱, 데몬, 세션 상태
#   stop        세션을 닫고 앱을 끄고 임시 파일을 모두 지운다
#
# 앱 위치를 바꾸려면 PEGBOARD_MANUAL_APP=<Pegboard.app>. PBM_HOOKS=1 start는 측정용이다(브라우저가
# 확인 창 기록을 /tmp/pbm/dialogs.jsonl에 남긴다. 자동으로 답하지는 않는다).
set -u
APP=${PEGBOARD_MANUAL_APP:-/Applications/Pegboard.app}
ST=/tmp/pbm                  # 짧은 경로(소켓 경로 길이 제한)
H=$ST/h; UDD=$ST/ud; WWW=$ST/www; UP=$ST/up
PORT=8765
PB=$APP/Contents/Helpers/pegboard
DEFAULT_UDD="$HOME/Library/Application Support/Pegboard"
DELAY=${PBM_DELAY:-5}

say() { print -r -- "$*"; }
die() { say "manual-3b: $*" >&2; exit 1; }
browser_pids() { pgrep -f -- "--user-data-dir=$UDD" 2>/dev/null | while read p; do
  ps -o command= -p $p | grep -q "^$APP/Contents/MacOS/" && echo $p; done; }
daemon_pid() { [ -f $H/daemon.pid ] && { p=$(<$H/daemon.pid); kill -0 $p 2>/dev/null && echo $p; }; }
pb() { PEGBOARD_HOME=$H "$PB" "$@"; }

wait_daemon() {  # until the daemon answers (up to 30 s)
  for i in {1..60}; do pb browser status >/dev/null 2>&1 && return 0; sleep 0.5; done
  die "데몬이 30초 안에 답하지 않는다 ($H/daemon.log)"
}

# session <key> [create options...]: the handle of this script's session <key>, made once.
session() {
  local key=$1; shift
  local f=$ST/session-$key
  if [ -f $f ] && pb session info "$(<$f)" >/dev/null 2>&1; then cat $f; return; fi
  local out
  out=$(pb session create --name "$key" --url "http://127.0.0.1:$PORT/" "$@") ||
    die "session create 실패: $out"
  print -r -- "${out%%$'\n'*}" > $f
  cat $f
}

countdown() {
  say "${DELAY}초 뒤에 확인 창이 뜹니다. 그 사이 사람 창(첫 창)을 눌러 글을 치고 있거나,"
  say "Finder 같은 다른 앱을 앞으로 가져와 보세요(그러면 Dock 아이콘이 한 번 튑니다)."
  for ((i = DELAY; i > 0; i--)); do printf '%s ' $i; sleep 1; done; say
}

ask() {  # ask <session key> <create options...> -- <command...>
  local key=$1; shift
  local opts=()
  while [ $# -gt 0 ] && [ "$1" != "--" ]; do opts+=("$1"); shift; done
  shift
  local h; h=$(session $key "${opts[@]}") || exit 1
  countdown
  say "보낸 명령: $*   (60초 안에 답하지 않으면 거부)"
  PEGBOARD_SESSION=$h pb "$@"
  say "-> 종료 코드 $?"
}

case "${1:-}" in
start)
  [ -d $APP ] || die "앱이 없다: $APP"
  [ -n "$(browser_pids)" ] && die "이미 켜져 있다 (stop 먼저)"
  rm -rf $ST; mkdir -p $H $UDD $WWW $UP; chmod 700 $ST
  # Chromium makes the default user data directory (Crashpad only) even with --user-data-dir;
  # stop removes it again if this start made it.
  [ -e "$DEFAULT_UDD" ] || touch $ST/made-default-udd
  cat > $WWW/index.html <<'EOF'
<!doctype html><meta charset=utf-8><title>업로드 시험 페이지</title>
<h1>업로드 시험 페이지</h1><p>에이전트가 아래 칸에 파일을 넣으려 합니다.</p>
<input type=file id=f>
EOF
  print '보고서 예시 파일' > $WWW/report.txt
  print '업로드 폴더의 파일' > $UP/notes.txt
  (cd $WWW && nohup python3 -m http.server $PORT --bind 127.0.0.1 > $ST/http.log 2>&1 & echo $! > $ST/http.pid)
  hooks=()   # PBM_HOOKS=1: measurement only (the browser logs its dialogs; nothing answers)
  [ "${PBM_HOOKS:-}" = 1 ] && hooks=(--env PEGBOARD_TEST_HOOKS=1 --env PEGBOARD_TEST_DIALOG_LOG=$ST/dialogs.jsonl)
  open -n -a "$APP" --env PEGBOARD_HOME=$H "${hooks[@]}" --args --user-data-dir=$UDD \
    --use-mock-keychain --disable-features=MacAppCodeSignClone \
    --no-first-run --no-default-browser-check || die "open 실패"
  wait_daemon
  say "켜졌다. 창 하나(사람 프로필)가 떴는지 본다. 그다음: upload, upload-dir, state, storage, giveup"
  ;;
upload)     ask 업로드 -- upload '#f' $WWW/report.txt ;;
upload-dir) ask 업로드폴더 --upload-dir $UP -- upload '#f' notes.txt ;;
state)      ask 쿠키 -- cookies set demo hello ;;
storage)    ask 저장소 -- storage local set demo-key demo-value ;;
giveup)
  [ -n "$(daemon_pid)" ] || wait_daemon
  say "데몬을 네 번 죽인다(앱은 1초 뒤 다시 띄우고, 1분 안에 네 번째면 포기하고 알림을 띄운다)."
  last=""
  for n in 1 2 3 4; do
    for i in {1..40}; do p=$(daemon_pid); [ -n "$p" ] && [ "$p" != "$last" ] && break; sleep 0.25; done
    [ -n "$p" ] && [ "$p" != "$last" ] || die "새 데몬이 뜨지 않았다(${n}번째)"
    kill -9 $p; say "  $n: 데몬 $p 죽임"; last=$p
    for i in {1..40}; do kill -0 $p 2>/dev/null || break; sleep 0.1; done
  done
  say "사람 창에 알림이 떠야 한다. \"다시 시작\"을 누르면 몇 초 안에 데몬이 다시 답한다(status로 확인)."
  ;;
status)
  say "앱: $(browser_pids | head -1 || true)   데몬: $(daemon_pid || echo 없음)"
  pb browser status 2>&1 | head -5
  pb session list 2>&1 | head -10
  ;;
stop)
  for f in $ST/session-*(N); do pb session close "$(<$f)" >/dev/null 2>&1; done
  for p in $(browser_pids); do kill $p 2>/dev/null; done
  for i in {1..40}; do [ -z "$(browser_pids)" ] && break; sleep 0.25; done
  for p in $(browser_pids); do kill -9 $p 2>/dev/null; done
  p=$(daemon_pid); [ -n "$p" ] && kill $p 2>/dev/null
  [ -f $ST/http.pid ] && kill $(<$ST/http.pid) 2>/dev/null
  sleep 1
  if [ -f $ST/made-default-udd ] && [ "$(ls -A "$DEFAULT_UDD" 2>/dev/null)" = Crashpad ]; then
    rm -rf "$DEFAULT_UDD"
  fi
  rm -rf $ST
  say "껐다. 남은 것: $(pgrep -fl -- "$UDD" | head -3)"
  ;;
*) sed -n 2,15p "$0"; exit 2 ;;
esac
