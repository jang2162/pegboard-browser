# 1단계 수동 확인 (M1~M7)

0단계에서 사람 손이 필요해 하지 못한 확인들이다([spike-findings.md](./spike-findings.md)의 "미실시"). 자동 시험은 이 항목들을 대신하지 못한다. 이 문서의 모든 "실제 결과" 칸은 비어 있다. 사용자가 직접 수행하고 채운다. 결과가 나오면 spike-findings.md의 "미실시" 표와 [roadmap.md](./roadmap.md)의 1단계 상태를 그에 맞춰 고친다.

수행자가 하는 일은 명령 붙여넣기, 화면 조작(잠금, 공간 이동, 디스플레이 끄기, 창 닫기), 결과 기록이다. 기대 결과와 다르면 그 자체가 발견이므로 숨기지 말고 칸에 그대로 적는다.

## 준비 (한 번만)

저장소 루트에서 실행한다.

```bash
cargo build --release
export PATH="$PWD/target/release:$PATH"

# 시험용 PEGBOARD_HOME: 평소 쓰는 데이터와 섞이지 않게 임시 디렉터리를 쓴다.
export PEGBOARD_HOME="$(mktemp -d /tmp/pegboard-manual.XXXX)"
echo "$PEGBOARD_HOME"

# 시험 페이지 (버튼을 누르면 숫자가 올라간다)와 내려받을 파일
mkdir -p /tmp/pegboard-manual-site && cd /tmp/pegboard-manual-site
cat > counter.html <<'HTML'
<!doctype html><meta charset="utf-8"><title>counter</title>
<button id="btn">Add</button> <span id="clicks">0</span>
<script>document.getElementById('btn').addEventListener('click', () => {
  const el = document.getElementById('clicks'); el.textContent = String(Number(el.textContent) + 1); });</script>
HTML
echo "pegboard manual check" > sample.zip
python3 -m http.server 8765 --bind 127.0.0.1 >/dev/null 2>&1 &
echo $! > /tmp/pegboard-manual-site/server.pid
cd - >/dev/null

pegboard browser start
```

세션을 만들 때 첫 줄에 `<id>.<토큰>` 핸들이 찍힌다. 핸들이 필요한 항목마다 아래처럼 만들고 환경변수로 넘긴다(명령줄 인자는 `ps`에 보인다).

```bash
export PEGBOARD_SESSION="$(pegboard session create --name m-check --url http://127.0.0.1:8765/counter.html | head -1)"
```

모든 확인이 끝나면 정리한다.

```bash
pegboard browser stop
kill "$(cat /tmp/pegboard-manual-site/server.pid)"
unset PEGBOARD_SESSION
rm -rf "$PEGBOARD_HOME" /tmp/pegboard-manual-site
```

## M1~M3 공통 반복 스크립트

10초 간격으로 `snapshot`과 `click`을 2분(12회) 반복하고, 끝나면 시험 페이지의 클릭 횟수와 동작 기록을 보여 준다. 터미널을 닫아도 계속 돌도록 `nohup`으로 띄운다. 화면 잠금이나 절전 중에도 이 셸 스크립트는 계속 돈다.

```bash
cat > /tmp/pegboard-m-loop.sh <<'LOOP'
#!/usr/bin/env bash
# PEGBOARD_HOME 과 PEGBOARD_SESSION 은 환경에서 받는다.
fail=0
for i in $(seq 1 12); do
  start=$(date +%s)
  if ! pegboard snapshot -i >/dev/null 2>/tmp/pegboard-m-err.txt; then
    echo "$(date +%T) #$i snapshot FAILED: $(head -c 200 /tmp/pegboard-m-err.txt)"; fail=$((fail+1))
  fi
  if ! pegboard click '#btn' >/dev/null 2>/tmp/pegboard-m-err.txt; then
    echo "$(date +%T) #$i click FAILED: $(head -c 200 /tmp/pegboard-m-err.txt)"; fail=$((fail+1))
  fi
  echo "$(date +%T) #$i done in $(( $(date +%s) - start )) s"
  sleep 10
done
echo "failures: $fail (of 24 commands)"
echo "clicks on the page: $(pegboard eval "document.getElementById('clicks').textContent" | grep '"result"')"
pegboard recording show "$(pegboard recording list --json | python3 -c 'import sys,json; print(json.load(sys.stdin)[-1]["recordingId"])')"
LOOP
chmod +x /tmp/pegboard-m-loop.sh
```

실행은 항목마다 다르다. 결과 로그는 `/tmp/pegboard-m-loop.log`에 쌓인다.

```bash
nohup /tmp/pegboard-m-loop.sh > /tmp/pegboard-m-loop.log 2>&1 &
```

끝나면(2분 조금 넘게 걸린다) 읽는다.

```bash
cat /tmp/pegboard-m-loop.log
```

기대 결과(M1~M3 공통): `failures: 0`, `clicks on the page:` 뒤에 `"result": "12"`, 동작 기록의 모든 줄이 `ok`이고 소요 시간(duration)이 평소(수십~수백 ms)와 비슷하다. 실패나 몇 초씩 걸리는 줄이 있으면 그것이 발견이다. 동작 기록은 세션 하나에 누적되므로 항목마다 새 세션을 만들고 로그 파일도 지운다(`rm -f /tmp/pegboard-m-loop.log`).

## 항목

### M1. 화면 잠금 중 자동화

| | |
|---|---|
| 목적 | 화면이 잠긴 동안에도 에이전트 창의 탭에서 CDP 자동화가 동작하는지(원격 데스크톱에서 흔한 상태) |
| 절차 | 1. 새 세션을 만든다(위 "세션을 만들 때"). 2. `nohup /tmp/pegboard-m-loop.sh > /tmp/pegboard-m-loop.log 2>&1 &` 로 띄운다. 3. 곧바로 화면을 잠근다(Control+Command+Q). 4. 2분 넘게 기다렸다가 잠금을 푼다. 5. `cat /tmp/pegboard-m-loop.log` |
| 기대 결과 | 공통 기대 결과와 같다. 잠금 중에 찍힌 반복도 실패하지 않는다 |
| 실제 결과 | |

### M2. 다른 데스크톱 공간

| | |
|---|---|
| 목적 | 에이전트 창이 다른 Space에 있어 보이지 않을 때의 동작 |
| 절차 | 1. 새 세션을 만든다. 2. 에이전트 창(탭그룹 `m-check`가 있는 창)을 Mission Control에서 다른 Space로 끌어 놓는다. 3. 사람 창이 있는 Space로 돌아온다(Control+방향키). 4. 같은 반복 스크립트를 띄운다. 5. 2분 넘게 기다린 뒤 로그를 읽는다 |
| 기대 결과 | 공통 기대 결과와 같다 |
| 실제 결과 | |

### M3. 디스플레이 절전

| | |
|---|---|
| 목적 | 화면이 꺼진 동안의 동작 |
| 절차 | 1. 새 세션을 만든다. 2. 반복 스크립트를 띄운다. 3. 곧바로 `pmset displaysleepnow` 로 디스플레이를 끈다. 4. 2분 넘게 기다린 뒤 키를 눌러 화면을 켠다(잠금 화면이 뜨면 풀어도 된다). 5. 로그를 읽는다 |
| 기대 결과 | 공통 기대 결과와 같다 |
| 실제 결과 | |

### M4. 브라우저 설정 누출

| | |
|---|---|
| 목적 | 사람 프로필에서 바꾼 환경설정이 에이전트 프로필 창에 새는지(A12) |
| 절차 | 1. 세션을 만든다(에이전트 창이 뜬다). 2. 사람 창(에이전트 창이 아닌 쪽)에서 `chrome://settings` 를 연다. 3. 외모에서 "홈 버튼 표시"를 켜고, 검색엔진을 바꾸고(예: DuckDuckGo), 다운로드에서 위치를 다른 폴더로 바꾼다. 4. 에이전트 창(탭그룹이 있는 창)에서 같은 설정 화면을 열고(메뉴의 설정) 세 항목을 본다. 5. 에이전트 창 툴바에 홈 버튼이 있는지도 본다 |
| 기대 결과 | 에이전트 창의 세 항목이 사람 창에서 바꾼 값으로 바뀌지 않는다(프로필별 설정). 바뀐 항목이 있으면 새는 설정이므로 이름을 적는다 |
| 실제 결과 | |

### M5. 다운로드 경로

| | |
|---|---|
| 목적 | 세션에서 받은 파일이 어디에 저장되는지. 설계는 세션별 디렉터리를 가정한다([architecture.md 6절](./architecture.md)) |
| 절차 | 1. 새 세션을 만든다. 2. `pegboard open http://127.0.0.1:8765/` 3. `pegboard snapshot -i` 로 `sample.zip` 링크의 ref를 찾아 `pegboard click @e3`(ref는 출력에 찍힌 번호대로) 4. 파일이 어디에 생겼는지 찾는다: `ls "$PEGBOARD_HOME/sessions"/*/files` 와 `ls -lt ~/Downloads \| head` 와, M4에서 바꾼 다운로드 폴더 |
| 기대 결과 | 파일이 `$PEGBOARD_HOME/sessions/<세션 id>/files` 아래에 생기거나, 최소한 사람이 쓰는 다운로드 폴더에 섞이지 않는다. 사람 쪽 폴더에 생기면 알려진 한계로 적는다 |
| 실제 결과 | |

### M6. 사람이 탭을 탭그룹 사이로 끌기

| | |
|---|---|
| 목적 | 사람이 에이전트 창 안에서 탭을 다른 세션의 그룹으로 옮겼을 때 세션 소속이 설계대로 바뀌는지(6절: 소속이 바뀐다) |
| 절차 | 1. 세션 A와 B를 만든다(각각 핸들을 `A`, `B` 변수에 담는다): `A=$(pegboard session create --name A --url http://127.0.0.1:8765/counter.html \| head -1)`, B도 같게. 2. 에이전트 창에서 A 그룹의 탭을 B 그룹으로 끌어다 놓는다. 3. `pegboard session info "$A"`, `pegboard session info "$B"` 4. `PEGBOARD_SESSION="$A" pegboard tab list` 와 `PEGBOARD_SESSION="$B" pegboard tab list` |
| 기대 결과 | 옮긴 탭이 B의 `tab list`와 B의 타깃 목록에 나타나고 A에서는 사라진다. A가 옮겨진 탭을 계속 조종하거나, 두 세션에 모두 보이면 실패다 |
| 실제 결과 | |

### M7. 에이전트 창 닫기

| | |
|---|---|
| 목적 | 사람이 에이전트 창을 닫았을 때 세션이 멈추지 않고, 탭이 엉뚱한 곳(사람 프로필)에 생기지 않는지 |
| 절차 | 1. 새 세션을 만든다. 2. 에이전트 창(탭그룹이 있는 창)을 닫는다. 3. `pegboard session info "$PEGBOARD_SESSION"` 4. `pegboard snapshot` 5. `pegboard open http://127.0.0.1:8765/counter.html` 6. `pegboard session close "$PEGBOARD_SESSION"` (모든 명령이 몇 초 안에 끝나는지, 오류 메시지가 무엇인지 본다) 7. 사람 창에 모르는 탭이 생겼는지 본다 |
| 기대 결과 | 명령이 멈추지 않고 몇 초 안에 응답한다. 창이 없다는 명확한 오류로 끝나거나 새 창과 탭을 에이전트 프로필에 열어 계속한다. 사람 프로필 창에 탭이 생기면 실패다. `session close`는 성공한다 |
| 실제 결과 | |

## 결과를 반영할 곳

- [spike-findings.md](./spike-findings.md)의 "미실시" 표에서 화면 잠금, 다른 데스크톱 공간, 환경설정 누출, 다운로드 경로 행: 수행한 날짜와 결과를 적는다.
- 새는 것이 나오면 [architecture.md](./architecture.md) 11절 "남는 위험"과 README의 "알려진 한계"에 한 줄씩 더한다.
- 7개가 모두 끝나면 [roadmap.md](./roadmap.md)의 1단계 상태를 "완료"로 고친다.
