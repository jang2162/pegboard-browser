# 1단계 수동 확인 (M1~M7, M8, M8b)

0단계에서 사람 손이 필요해 하지 못한 확인들이다([spike-findings.md](./spike-findings.md)의 "미실시"). 자동 시험은 이 항목들을 대신하지 못한다. 이 문서의 모든 "실제 결과" 칸은 비어 있다. 사용자가 직접 수행하고 채운다. 결과가 나오면 spike-findings.md의 "미실시" 표와 [roadmap.md](./roadmap.md)의 1단계 상태를 그에 맞춰 고친다.

수행자가 하는 일은 명령 붙여넣기, 화면 조작(잠금, 공간 이동, 디스플레이 끄기, 창 닫기), 결과 기록이다. 기대 결과와 다르면 그 자체가 발견이므로 숨기지 말고 칸에 그대로 적는다.

## 한눈에 보기

| # | 확인하는 것 | 화면 조작 | 데스크톱을 조작하는 에이전트가 할 수 있나 |
|---|---|---|---|
| M1 | 화면 잠금 중 자동화 | 잠그기, 풀기 | 일부. 반복 실행과 잠그기는 되지만 잠금 해제(암호)는 사람이 한다 |
| M2 | 다른 데스크톱 공간 | 창을 다른 Space로 옮기기 | 일부. Mission Control 끌기는 에이전트에게 불안정하다. 사람이 창만 옮겨 주면 나머지는 된다 |
| M3 | 디스플레이 절전 | 화면 끄기, 켜기 | 된다(`pmset displaysleepnow`, 깨우기는 `caffeinate -u -t 1`). 깨울 때 암호를 묻는 설정이면 사람이 푼다 |
| M4 | 브라우저 설정 누출 | 설정 화면 클릭 | 된다 |
| M5 | 다운로드 경로 | 없음 | 된다. 명령과 `ls`뿐이라 데스크톱 조작도 필요 없다 |
| M6 | 탭을 다른 그룹으로 끌기 | 탭 끌어다 놓기 | 된다. 끌기가 제대로 됐는지 화면으로 확인한 뒤 진행한다 |
| M7 | 에이전트 창 닫기 | 창 닫기 | 된다 |
| M8 | 사람 창 클릭 뒤 `tab new` | 사람 창 클릭 | 된다. 에이전트의 클릭도 운영체제 수준의 클릭이다 |
| M8b | 맨 앞 앱이 바뀌지 않는지 | 다른 앱에서 타이핑 | 된다. 아래의 맨 앞 앱 기록 명령을 쓰면 눈으로 보는 것보다 정확하다 |

에이전트에게 맡길 때 지킬 것:

- 여기서 말하는 에이전트는 화면을 보고 마우스와 키보드를 움직이는 도구(컴퓨터 사용)를 가진 에이전트다. pegboard 세션으로 브라우저만 조종하는 에이전트는 이 확인을 할 수 없다. macOS의 손쉬운 사용과 화면 기록 권한이 필요하다.
- 에이전트가 한 항목은 "실제 결과" 칸에 "에이전트 수행"이라고 적고, 판단 근거(명령 출력, 화면에서 본 것)를 함께 남긴다.
- 화면이 잠긴 동안 에이전트는 화면을 보지도 조작하지도 못한다. M1은 사람이 잠금을 풀어 준 뒤에 에이전트가 로그를 읽는다.
- 에이전트가 다른 계정이 로그인된 브라우저를 건드리지 않게, 이 확인은 시험용 `PEGBOARD_HOME`의 Chrome for Testing 창에서만 한다.

권장 순서: M5(조작 없음) → M4 → M7 → M8 → M8b → M6 → M3 → M2 → M1. 뒤로 갈수록 사람 손이 더 필요하다.

## 새 기기에서 시작할 때

macOS arm64, Rust stable, git이 필요하다. 저장소를 받은 뒤 한 번만 한다.

```bash
git clone <저장소 주소> pegboard-browser && cd pegboard-browser   # main에 1단계가 들어 있다
scripts/fetch-agent-browser.sh                    # agent-browser 0.42.0을 받아 체크섬을 확인한다
vendor/agent-browser/agent-browser install        # Chrome for Testing을 받는다
cargo test --workspace                            # 브라우저 없이 도는 시험이 통과하는지 본다
```

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
| 목적 | 사람이 에이전트 창 안에서 탭을 다른 세션의 그룹으로 옮겼을 때 1단계의 동작이 그대로인지: 소속은 탭을 따라가지 그룹을 따라가지 않는다(세션의 범위는 타깃 ID로 정해진다). 그룹을 따라 소속을 바꾸는 것은 2단계에서 정한다 |
| 절차 | 1. 세션 A와 B를 만든다(각각 핸들을 `A`, `B` 변수에 담는다): `A=$(pegboard session create --name A --url http://127.0.0.1:8765/counter.html \| head -1)`, B도 같게. 2. 에이전트 창에서 A 그룹의 탭을 B 그룹으로 끌어다 놓는다. 3. `pegboard session info "$A"`, `pegboard session info "$B"` 4. `PEGBOARD_SESSION="$A" pegboard tab list` 와 `PEGBOARD_SESSION="$B" pegboard tab list` |
| 기대 결과 | 옮긴 탭은 계속 A의 것이다. A의 `tab list`와 `session info "$A"`에 그대로 남고 A가 계속 조종할 수 있다. B의 `tab list`와 `session info "$B"`에는 나타나지 않는다. 그 탭이 B의 그룹 안에 있으므로 `pegboard session close "$B"`를 하면 B의 그룹 구성원으로서 함께 닫힌다(A는 그 탭을 잃는다). B가 그 탭을 조종하거나 보면 실패다(격리 위반). 위 동작이 아니면(예: A의 목록에서 사라짐) 코드와 문서가 어긋난 것이니 기록한다 |
| 실제 결과 | |

### M7. 에이전트 창 닫기

| | |
|---|---|
| 목적 | 사람이 에이전트 창을 닫았을 때 세션이 멈추지 않고, 탭이 엉뚱한 곳(사람 프로필)에 생기지 않는지 |
| 절차 | 1. 새 세션을 만든다. 2. 에이전트 창(탭그룹이 있는 창)을 닫는다. 3. `pegboard session info "$PEGBOARD_SESSION"` 4. `pegboard snapshot` 5. `pegboard open http://127.0.0.1:8765/counter.html` 6. `pegboard session close "$PEGBOARD_SESSION"` (모든 명령이 몇 초 안에 끝나는지, 오류 메시지가 무엇인지 본다) 7. 사람 창에 모르는 탭이 생겼는지 본다 |
| 기대 결과 | 명령이 멈추지 않고 몇 초 안에 응답한다. 창이 없다는 명확한 오류로 끝나거나 새 창과 탭을 에이전트 프로필에 열어 계속한다. 사람 프로필 창에 탭이 생기면 실패다. `session close`는 성공한다 |
| 실제 결과 | |

### M8. 사람 창을 클릭해 앞으로 가져온 뒤 `tab new`

| | |
|---|---|
| 목적 | 자동 시험 `new_tabs_always_open_in_the_agent_profile`(와 core의 `created_tab_lands_in_agent_profile_even_when_human_window_is_active`)는 "사람 창이 마지막으로 활성인 창"이라는 전제를 만든 뒤 탭을 만든다. 전제는 컨텍스트 없는 `Target.createTarget`이 사람 프로필에 생기는지로 확인한다. `Target.activateTarget`만으로는 전제가 만들어지지 않았고(OS 포커스가 다른 앱에 있을 때, Chrome 157), Chrome을 시험 홈의 user-data-dir로 다시 실행해 새 사람 창을 여는 방식으로 만들었다. 이 항목은 실제 사람의 클릭으로 같은 확인을 한다 |
| 절차 | 1. 세션을 만든다. 2. 사람 창(에이전트 창이 아닌 쪽)의 탭 영역을 마우스로 직접 클릭해 활성 창으로 만든다. 3. 바로 `pegboard tab new http://127.0.0.1:8765/counter.html` 4. 사람 창에 탭이 생겼는지, 에이전트 창의 세션 그룹에 생겼는지 본다. 5. `pegboard tab list` 6. 사람 창을 다시 클릭하고 `pegboard tab new about:blank`를 반복한다 |
| 기대 결과 | 새 탭은 항상 에이전트 창의 세션 그룹에 생기고 사람 창의 탭 수는 그대로다 |
| 실제 결과 | |

### M8b. 에이전트 동작 중 다른 앱에서 타이핑이 끊기지 않는지

| | |
|---|---|
| 목적 | 운영체제 수준의 포커스 도둑질 확인. 자동 시험 `agent_actions_do_not_change_the_humans_active_tab`은 브라우저 안의 상태(사람 창의 활성 탭, `document.hasFocus()`)만 증명하고 맨 앞 앱이 바뀌는지는 증명하지 못한다 |
| 절차 | 1. 세션을 만든다(위 "세션을 만들 때"와 같은 방식으로 `PEGBOARD_SESSION`을 설정). 2. 텍스트 편집기나 메모 앱을 맨 앞에 두고 그 글상자에 글자를 계속 입력한다(멈추지 않고). 3. 입력을 계속하는 동안 다른 터미널 창에서 아래를 붙여넣어 실행한다(약 1분). 4. 입력이 끊기거나 맨 앞 앱이 Chrome for Testing으로 바뀌는 순간이 있었는지 본다. 눈으로 보는 대신 아래의 맨 앞 앱 기록 명령을 먼저 띄워 두면 바뀐 순간이 파일에 남는다 |
| 기대 결과 | 입력이 끊기지 않고 맨 앞 앱이 바뀌지 않는다. 에이전트 창이 사람 창 앞으로 올라오지도 않는다 |
| 실제 결과 | |

```bash
for i in $(seq 1 20); do
  pegboard open 'data:text/html,<input id=t><button id=b>go</button>' >/dev/null
  pegboard fill '#t' "agent $i" >/dev/null
  pegboard click '#b' >/dev/null
  pegboard tab new http://127.0.0.1:8765/counter.html >/dev/null
  pegboard tab close >/dev/null
  sleep 2
done
echo done
```

맨 앞 앱 기록(위 반복을 돌리기 전에 다른 터미널에서 띄운다. 0.2초마다 맨 앞 앱 이름을 적고, 반복이 끝나면 Control+C로 멈춘다):

```bash
while true; do echo "$(date +%T) $(lsappinfo info -only name "$(lsappinfo front)" | head -1)"; sleep 0.2; done > /tmp/pegboard-front.log
# 끝난 뒤: 맨 앞이었던 앱의 종류와 횟수. 타이핑하던 앱 하나만 나와야 한다.
cut -d' ' -f2- /tmp/pegboard-front.log | sort | uniq -c
```

## 결과를 반영할 곳

- [spike-findings.md](./spike-findings.md)의 "미실시" 표에서 화면 잠금, 다른 데스크톱 공간, 환경설정 누출, 다운로드 경로 행: 수행한 날짜와 결과를 적는다.
- 새는 것이 나오면 [architecture.md](./architecture.md) 11절 "남는 위험"과 README의 "알려진 한계"에 한 줄씩 더한다.
- M1~M7, M8, M8b가 모두 끝나면 [roadmap.md](./roadmap.md)의 1단계 상태를 "완료"로 고친다.
