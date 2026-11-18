# 2b단계 수동 확인 (P1~P8)

2b(녹화와 재생) 자동 시험이 덮지 못하는 것이다. 사람의 눈과 손, 화면 잠금이나 다른 Space 같은 데스크톱 상태, 레티나 화면이 필요하다. 아직 수행하지 않았다. 수행하면 "실제 결과" 칸을 채우고, 기대와 다르면 README의 "알려진 한계"(녹화)와 [architecture.md](./architecture.md) 11절 "남는 위험"에 한 줄씩 더한다. P3, P4, P5, P7은 측정이 비어 있는 것이므로 결과를 [spike-findings.md](./spike-findings.md) "2b단계 측정"의 V1, H, F3 줄에도 적는다.

데스크톱을 조작하는 에이전트에게 맡길 때 지킬 것은 [phase1-manual-checks.md](./phase1-manual-checks.md)와 같다. P5(사람 개입)는 **사람이 직접** 한다. 에이전트의 합성 입력은 작업 1에서 이미 쟀고, 빠진 것이 사람의 손이다.

## 한눈에 보기

| # | 확인하는 것 | 화면 조작 | 사람이 꼭 필요한가 |
|---|---|---|---|
| P1 | 재생 화면을 눈으로: 첫 사고, 표시, 키, 탭, 화면 없음·꺼짐 글 | 브라우저에서 누르기 | 눈으로 판단한다 |
| P2 | 에이전트 창을 가리거나 최소화한 채 녹화한 화면 | 창 최소화, 다른 창으로 덮기 | 아니다 |
| P3 | 화면 잠금과 디스플레이 절전 중의 녹화 | 잠그기, 풀기, 화면 끄기 | 잠금 해제 |
| P4 | 다른 Space에 있는 에이전트 창의 녹화 | 창을 다른 Space로 옮기기 | Space 옮기기 |
| P5 | 사람이 직접 한 주소창 입력, 뒤로, 링크, 탭 전환의 사람 개입 표시 (H1~H5) | 에이전트 창에서 직접 | **그렇다** |
| P6 | 화면을 끈 도메인의 화면이 재생에 없는지 | 없음(눈으로 확인) | 눈으로 판단한다 |
| P7 | 레티나 화면에서 캡처 크기와 명령 지연 | 없음 | 레티나 Mac이 있어야 한다 |
| P8 | 2a 클립보드 시험을 텍스트 클립보드로 다시 돌리기 | 없음 | 아니다 |

권장 순서: P8(조작 없음) → P7 → P6 → P1 → P2 → P5 → P4 → P3.

## 준비 (한 번만)

[phase1-manual-checks.md](./phase1-manual-checks.md)의 "새 기기에서 시작할 때"를 먼저 한다. 그다음 저장소 루트에서:

```bash
cargo build --release
export PATH="$PWD/target/release:$PATH"
export PEGBOARD_HOME="$(mktemp -d /tmp/pegboard-2b.XXXX)"
echo "$PEGBOARD_HOME"

# 화면을 끄는 도메인: localhost. 시험 페이지는 127.0.0.1과 localhost 두 이름으로 같은 서버에 닿는다.
cat > "$PEGBOARD_HOME/config.toml" <<'TOML'
[recording]
screen_off_domains = ["localhost"]
TOML

mkdir -p /tmp/pegboard-2b-site && cd /tmp/pegboard-2b-site
# 배경색이 0.2초마다 바뀌는 페이지 (화면이 늘 바뀐다), 버튼과 링크
cat > anim.html <<'HTML'
<!doctype html><meta charset="utf-8"><title>anim</title>
<h1 id="t">ANIM</h1>
<button id="btn">Add</button> <span id="clicks">0</span>
<p><a id="to-counter" href="counter.html">counter</a> · <a id="to-off" href="http://localhost:8765/off.html">off page</a></p>
<script>
let n = 0; setInterval(() => { document.body.style.background = `hsl(${(n++ * 37) % 360} 70% 80%)`; }, 200);
document.getElementById('btn').addEventListener('click', () => {
  const el = document.getElementById('clicks'); el.textContent = String(Number(el.textContent) + 1); });
</script>
HTML
cat > counter.html <<'HTML'
<!doctype html><meta charset="utf-8"><title>counter</title>
<h1>COUNTER</h1><button id="btn">Add</button> <span id="clicks">0</span>
<p><a id="back-anim" href="anim.html">anim</a></p>
<script>document.getElementById('btn').addEventListener('click', () => {
  const el = document.getElementById('clicks'); el.textContent = String(Number(el.textContent) + 1); });</script>
HTML
# 화면을 끈 도메인에서 볼 페이지: 크게 OFF PAGE
cat > off.html <<'HTML'
<!doctype html><meta charset="utf-8"><title>off</title>
<body style="background:#c00;color:#fff"><h1 style="font-size:96px">OFF PAGE</h1>
<p><a id="out" href="http://127.0.0.1:8765/anim.html">back to anim</a></p></body>
HTML
# 끈 도메인을 iframe으로 띄우는 페이지 (알려진 한계 확인용)
cat > frame.html <<'HTML'
<!doctype html><meta charset="utf-8"><title>frame</title>
<h1>FRAME HOST</h1><iframe src="http://localhost:8765/off.html" width="600" height="300"></iframe>
HTML
python3 -m http.server 8765 --bind 127.0.0.1 >/dev/null 2>&1 &
echo $! > /tmp/pegboard-2b-site/server.pid
cd - >/dev/null

pegboard browser start
```

녹화를 읽는 도구 둘(가장 새 녹화의 ID, 녹화의 동작과 화면 요약):

```bash
latest_rec() { pegboard recording list --json | python3 -c 'import sys,json; print(json.load(sys.stdin)["recordings"][-1]["recordingId"])'; }

cat > /tmp/pegboard-2b-frames.py <<'PY'
# 사용: python3 /tmp/pegboard-2b-frames.py <기록 id>  (PEGBOARD_HOME 필요)
import json, os, sys, collections
home = os.environ["PEGBOARD_HOME"]; rid = sys.argv[1]
d = os.path.join(home, "recordings", rid)
m = json.load(open(os.path.join(d, "manifest.json")))
print(f"recording {rid}  screen={m.get('screen')}  started={m['startedAt']}  ended={m.get('endedAt')}")
def lines(p):
    try:
        for l in open(p, encoding="utf-8"):
            l = l.strip()
            if l: yield json.loads(l)
    except FileNotFoundError:
        return
def has(name):
    return bool(name) and os.path.isfile(os.path.join(d, "screen", name))
print("\n# actions (seq ts kind command ok ms frameBefore frameAfter)")
for a in sorted(lines(os.path.join(d, "actions.jsonl")), key=lambda a: a["ts"]):
    if a.get("kind") == "human":
        print(f"{a['seq']:>3} {a['ts'][11:23]} HUMAN {a.get('what')}  certain={a.get('certain')}  {a.get('url')}")
        continue
    fb, fa = a.get("frameBefore"), a.get("frameAfter")
    print(f"{a['seq']:>3} {a['ts'][11:23]} {a['command']:<10} ok={a['ok']!s:<5} {a.get('durationMs'):>5} ms"
          f"  before={'yes' if has(fb) else ('MISSING' if fb else '-')}  after={'yes' if has(fa) else ('MISSING' if fa else '-')}")
print("\n# screen per tab: frames per 10 s (change/after), markers, gaps over 3 s")
sd = os.path.join(d, "screen")
for t in sorted(os.listdir(sd)) if os.path.isdir(sd) else []:
    idx = list(lines(os.path.join(sd, t, "index.jsonl")))
    buckets = collections.OrderedDict()
    prev = None
    for l in idx:
        if l["kind"] in ("off", "capped"):
            print(f"  {t[:8]} {l['ts'][11:23]} MARK {l['kind']} {l.get('url') or ''}")
            continue
        c = buckets.setdefault(l["ts"][11:18] + "0", [0, 0]); c[0 if l["kind"] == "change" else 1] += 1
        now = int(l["ts"][11:13]) * 3600 + int(l["ts"][14:16]) * 60 + float(l["ts"][17:19])
        if prev is not None and now - prev > 3:
            print(f"  {t[:8]} gap {now - prev:.0f} s before {l['ts'][11:19]}")
        prev = now
    print(f"  tab {t[:8]}: " + "  ".join(f"{k}={c[0]}/{c[1]}" for k, c in buckets.items()))
    hosts = collections.Counter((l.get("url") or "").split("/")[2] if "//" in (l.get("url") or "") else "-" for l in idx if l.get("file"))
    print(f"  tab {t[:8]} frame hosts: {dict(hosts)}")
PY
```

세션마다 새로 만들고(`export PEGBOARD_SESSION="$(pegboard session create --name <이름> --url http://127.0.0.1:8765/anim.html | head -1)"`), 확인이 끝나면 `pegboard session close "$PEGBOARD_SESSION"`으로 닫는다(녹화가 마감된다). 반복 명령이 필요한 항목은 [phase1-manual-checks.md](./phase1-manual-checks.md)의 "M1~M3 공통 반복 스크립트"(`/tmp/pegboard-m-loop.sh`, 10초 간격으로 `snapshot`과 `click '#btn'` 12회)를 그대로 쓴다. 그 스크립트를 먼저 만들어 둔다.

모든 확인이 끝나면 정리한다.

```bash
pegboard browser stop
kill "$(cat /tmp/pegboard-2b-site/server.pid)"
unset PEGBOARD_SESSION
rm -rf "$PEGBOARD_HOME" /tmp/pegboard-2b-site /tmp/pegboard-2b-frames.py
```

## 항목

### P1. 재생 화면을 눈으로

| | |
|---|---|
| 목적 | 재생 화면이 설계 4.8절대로 보이는지. 자동 시험(`e2e_replay`)은 심은 녹화로 DOM을 읽는다. 사람이 실제 녹화를 보고 쓸 만한지는 보지 않는다 |
| 절차 | 1. `export PEGBOARD_SESSION="$(pegboard session create --name p1 --url http://127.0.0.1:8765/anim.html \| head -1)"` 2. `pegboard click '#btn'; pegboard click '#btn'` 3. `pegboard click '#missing'` (실패) 4. `pegboard cookies set a b` (거부, `policy_denied`) 5. `pegboard tab new http://127.0.0.1:8765/counter.html; pegboard click '#btn'` 6. `pegboard open http://localhost:8765/off.html; sleep 2; pegboard open http://127.0.0.1:8765/anim.html` 7. 2초 기다린 뒤 사람이 에이전트 창에서 anim 페이지가 보이는 탭의 "counter" 링크를 누른다(사람 개입). 3초 기다린다. 8. `pegboard session close "$PEGBOARD_SESSION"` 9. `R=$(latest_rec); python3 /tmp/pegboard-2b-frames.py $R` 10. 화면 없음을 보려고 프레임 하나를 지운다: `f=$(ls "$PEGBOARD_HOME/recordings/$R/screen"/*/0*.jpg \| head -3 \| tail -1); rm "$f"; echo "$f"` 11. `pegboard recording open $R` 12. 열린 페이지에서: 시작 위치를 본다. 시간축의 ✗를 한 번, 한 번 더 누른다. ⛔와 👤도 같이. 좌우 화살표 키를 몇 번 누른다. 탭 단추를 바꾼다. 6의 off 페이지 동작과 10에서 지운 프레임의 동작으로 간다. 13. 31분 뒤 같은 탭을 새로 고친다 |
| 기대 결과 | 11: 기본 브라우저(에이전트 창이 아님)에 페이지가 열리고 터미널에 `opened <id> in the default browser (the link works for 30 minutes)`. 머리글에 `세션 "p1"`, 명령·실패 1·거부 1·사람 개입 1 이상. 12: 처음에 `click #missing`이 골라져 있고 그 직전 화면(anim 페이지)이 보인다. ✗를 한 번 누르면 직전, 같은 표시를 또 누르면 직후 화면. ⛔ 동작은 프레임 이름이 없어 그 시각의 탭 화면이 보인다. 👤는 "사람 개입: … (가능성)". 화살표로 동작이 하나씩 움직인다. 탭 단추에 `127.0.0.1` 두 개(또는 탭마다 호스트)가 있고 바꾸면 그 탭의 화면이 나온다. off 페이지에 있던 동작에서는 그림 대신 "화면 기록 꺼짐", 지운 프레임에서는 "화면 없음". 빨간 "OFF PAGE" 그림은 어디에도 없다. `fill`처럼 가린 값이 있으면 `[redacted]`와 🔒. 13: "녹화를 열 수 없습니다…" 안내(링크 30분). 보기 불편하거나 헷갈리는 곳을 그대로 적는다 |
| 실제 결과 | 미수행 |

### P2. 가리거나 최소화한 에이전트 창

| | |
|---|---|
| 목적 | 작업 1(F4)은 스파이크의 Chrome으로 쟀다. 실제 pegboard 녹화기로, 사람이 다른 일을 하는 동안 가려진 창의 화면이 남는지 |
| 절차 | 1. `export PEGBOARD_SESSION="$(pegboard session create --name p2 --url http://127.0.0.1:8765/anim.html \| head -1)"` 2. 에이전트 창을 최소화한다(노란 단추). 3. `rm -f /tmp/pegboard-m-loop.log; nohup /tmp/pegboard-m-loop.sh > /tmp/pegboard-m-loop.log 2>&1 &` 4. 1분 뒤 창을 되살리고, 다른 앱 창(예: 터미널을 최대화)으로 에이전트 창을 완전히 덮는다. 5. 반복이 끝나면(2분 남짓) `pegboard session close "$PEGBOARD_SESSION"; python3 /tmp/pegboard-2b-frames.py $(latest_rec)` 6. `pegboard recording open $(latest_rec)`로 최소화·가림 동안의 화면을 본다 |
| 기대 결과 | `failures: 0`. 모든 `snapshot`, `click`에 `after=yes`. 10초 칸마다 스트림 프레임(`change`)이 있다(최소화 중에는 적을 수 있다. F4는 초당 약 4장). 재생 화면의 그림이 실제 페이지(색이 바뀌는 배경, 늘어나는 숫자)다. 명령 시간이 평소와 비슷하다 |
| 실제 결과 | 미수행 |

### P3. 화면 잠금과 디스플레이 절전

| | |
|---|---|
| 목적 | 잠금과 절전 중에 화면 스트림과 직후 캡처가 오는지, 명령이 느려지지 않는지. 작업 1에서 재지 못했다(spike-findings V1 "미측정", 설계 5절) |
| 절차 | 잠금: 1. `export PEGBOARD_SESSION="$(pegboard session create --name p3-lock --url http://127.0.0.1:8765/anim.html \| head -1)"` 2. `rm -f /tmp/pegboard-m-loop.log; nohup /tmp/pegboard-m-loop.sh > /tmp/pegboard-m-loop.log 2>&1 &` 3. 곧바로 화면을 잠근다(Control+Command+Q). 4. 2분 넘게 기다렸다가 잠금을 푼다. 5. `cat /tmp/pegboard-m-loop.log; pegboard session close "$PEGBOARD_SESSION"; python3 /tmp/pegboard-2b-frames.py $(latest_rec)` 절전: 6. 새 세션 `p3-sleep`으로 1~2를 하고 곧바로 `pmset displaysleepnow`. 7. 2분 뒤 키를 눌러 깨운다(암호를 물으면 푼다). 8. 5와 같다. 스파이크(원자료): 9. `cd spikes && npm ci && node 40-screencast.mjs --auto --with-lock --with-sleep --only=locked,display-sleep` (스크립트가 잠그고 알림음을 낸다. 사람이 잠금을 푼다. 10분까지 기다린다) |
| 기대 결과 | 명령: `failures: 0`, 소요 시간이 평소와 비슷하다(직후 캡처가 500ms 제한에 걸리면 한 명령이 500ms쯤 늘고 그 탭은 30초 동안 건너뛴다). 화면: 측정이 없으므로 그대로 적는다: 잠긴 동안 `after=yes`인지 `-`인지, 10초 칸의 `change` 수, 재생 화면에서 그 구간의 그림이 페이지인지 "화면 없음"인지. 화면이 남지 않으면 README 알려진 한계의 "다른 Space, 화면 잠금, 디스플레이 절전" 줄을 결과로 바꾼다. 9의 표(V1, V2 행)를 spike-findings에 옮긴다 |
| 실제 결과 | 미수행 |

### P4. 다른 Space

| | |
|---|---|
| 목적 | 에이전트 창이 다른 Space에 있을 때의 녹화. 작업 1에서는 Space를 만들지 못해 재지 못했다 |
| 절차 | 준비: Mission Control에 Space를 두 개 이상 만들고, 키보드 단축키(Control+←/→)를 켠다. 1. `export PEGBOARD_SESSION="$(pegboard session create --name p4 --url http://127.0.0.1:8765/anim.html \| head -1)"` 2. 에이전트 창을 다른 Space로 옮긴다(Mission Control에서 끌기). 지금 Space에 그 창이 보이지 않는지 본다. 3. `rm -f /tmp/pegboard-m-loop.log; nohup /tmp/pegboard-m-loop.sh > /tmp/pegboard-m-loop.log 2>&1 &` 4. 끝나면 `cat /tmp/pegboard-m-loop.log; pegboard session close "$PEGBOARD_SESSION"; python3 /tmp/pegboard-2b-frames.py $(latest_rec)` 5. 스파이크: `cd spikes && node 40-screencast.mjs --auto --only=other-space` |
| 기대 결과 | P3과 같다(명령은 실패하지 않고, 화면은 결과를 그대로 적는다). 5가 SKIPPED가 아니고 V1, V2 행을 낸다 |
| 실제 결과 | 미수행 |

### P5. 사람이 직접 한 동작의 사람 개입 표시 (H1~H5)

| | |
|---|---|
| 목적 | 사람이 에이전트 창에서 직접 한 주소창 입력(H1), 뒤로 단추(H2), 링크 클릭(H3), 탭 전환(H4)이 "사람 개입 가능성"으로 남는지, 다른 앱에 갔다 온 것(H5)은 남지 않는지. 작업 1은 운영체제 합성 입력만 썼다(F6) |
| 절차 | **사람이 마우스와 키보드로 한다.** 동작 사이에 2초 넘게 쉰다(명령 뒤 1초 안의 일은 명령의 것으로 본다). 1. `export PEGBOARD_SESSION="$(pegboard session create --name p5 --url http://127.0.0.1:8765/anim.html \| head -1)"` 2. `pegboard tab new http://127.0.0.1:8765/counter.html` 3. 5초 쉰다. 에이전트 창의 탭줄에서 첫 탭(anim)을 마우스로 누른다 (H4) 4. 그 탭의 주소창을 누르고 `127.0.0.1:8765/counter.html`을 쳐서 Return (H1) 5. 브라우저의 뒤로 단추를 마우스로 누른다 (H2) 6. 페이지의 "counter" 링크를 누른다 (H3) 7. Finder를 눌렀다가 다시 에이전트 창을 누른다 (H5) 8. 3초 쉬고 `pegboard get url` (사람 동작 뒤의 명령) 9. `pegboard recording show $(latest_rec)` 10. `pegboard session close "$PEGBOARD_SESSION"` 11. 스파이크(원자료, 사람이 안내대로 H1~H5를 한다, 약 3분): `cd spikes && node 41-human-signals.mjs` |
| 기대 결과 | 9의 출력에 `human:` 줄이 시간 순서로: H4 `switched to this tab (possibly)`, H1 `navigated outside a command: http://127.0.0.1:8765/counter.html (possibly)`, H2 `navigated outside a command: http://127.0.0.1:8765/anim.html (possibly)`, H3 `followed a link or form outside a command (possibly)`. H5에는 줄이 없다(앱 전환은 표시하지 않는다. 다시 누른 탭이 이미 앞 탭이면 탭 전환도 없다). 1과 2(명령)에는 `human:` 줄이 없다. 빠진 것이나 더 생긴 것을 그대로 적는다. 11의 표를 spike-findings "H"에 "사람" 행으로 더한다(뒤로 단추를 실제로 눌렀는지 포함) |
| 실제 결과 | 미수행 |

### P6. 화면을 끈 도메인

| | |
|---|---|
| 목적 | `screen_off_domains`의 사이트에서 일한 녹화에 그 화면이 정말 없는지 눈으로. 사람이 링크로 들어가고 나오는 경우도. iframe은 알려진 한계대로 찍히는지 |
| 절차 | 1. `export PEGBOARD_SESSION="$(pegboard session create --name p6 --url http://127.0.0.1:8765/anim.html \| head -1)"` 2. `pegboard open http://localhost:8765/off.html; pegboard click '#out'; sleep 2` (명령으로 들어가고 나온다) 3. 사람이 에이전트 창에서 "off page" 링크를 누르고, 3초 뒤 "back to anim" 링크를 누른다. 4. `pegboard open http://localhost:8765/off.html; pegboard get url; pegboard open http://127.0.0.1:8765/anim.html` 5. `pegboard open http://127.0.0.1:8765/frame.html; sleep 2` 6. `pegboard session close "$PEGBOARD_SESSION"; R=$(latest_rec); python3 /tmp/pegboard-2b-frames.py $R` 7. `grep -l localhost "$PEGBOARD_HOME/recordings/$R/screen"/*/index.jsonl` 8. `pegboard recording open $R`로 처음부터 끝까지 넘겨 본다 |
| 기대 결과 | 6: off 페이지에 있던 명령(`open …off.html`의 직후, `get url`, `click #out`의 직전)은 `after=-`/`before=-`, `MARK off` 줄이 있다. frame hosts에 `localhost:8765`가 없다. 7: 출력이 없거나, 있다면 `off` 표시 줄의 주소만이다(`file`이 있는 줄에 localhost가 없어야 한다: `grep '"file"' … \| grep localhost`가 비어 있다). 8: 빨간 "OFF PAGE" 그림이 frame.html 안의 iframe 말고는 어디에도 없다. frame.html 화면에는 iframe 안의 OFF PAGE가 보인다(알려진 한계: 주 프레임 주소로만 판정). 끈 동안의 동작에는 "화면 기록 꺼짐" |
| 실제 결과 | 미수행 |

### P7. 레티나 화면 (가능하면)

| | |
|---|---|
| 목적 | 배율 2 화면에서 직후 캡처가 가로 1280 이하로 줄어드는지, 크기와 명령 지연이 배율 1의 측정과 비슷한지. 레티나 기기가 없어 재지 못했다(F3, 설계 4.6절) |
| 절차 | 레티나 Mac에서(`system_profiler SPDisplaysDataType \| grep -i retina`로 확인. 외장 모니터가 배율 1이면 창을 내장 화면에 둔다). 1. 준비의 시험 페이지로 `export PEGBOARD_SESSION="$(pegboard session create --name p7 --url http://127.0.0.1:8765/anim.html \| head -1)"`, `pegboard eval "devicePixelRatio"`가 2인지 본다. 2. `pegboard click '#btn'` 다섯 번, `pegboard session close "$PEGBOARD_SESSION"` 3. `R=$(latest_rec); for f in $(ls "$PEGBOARD_HOME/recordings/$R/screen"/*/0*.jpg \| head -5); do sips -g pixelWidth -g pixelHeight "$f" \| tail -2; ls -l "$f" \| awk '{print $5}'; done` 4. `grep '"after"' "$PEGBOARD_HOME/recordings/$R/screen"/*/index.jsonl \| head -3` 5. 저장소 루트에서 `cargo test -p pegboard-cli --release --test e2e_recording recording_overhead_meets_the_targets -- --ignored --nocapture` (창을 레티나 화면에 둔 채) 6. 스파이크: `cd spikes && node 40-screencast.mjs --only=v3` 와 `node 42-disk.mjs --minutes 10` |
| 기대 결과 | 3: 그림 가로가 1280 이하(스트림 프레임과 `after` 캡처 모두). 4: `width`가 1280 이하. 5: 통과(명령마다 중앙값 60ms 이하, 최대 1초 이하). 표를 설계 4.6절과 spike-findings "작업 4"에 "레티나" 열로 더한다. 6: V3 표와 10분 디스크 양(배율 1의 30분 197MB와 비교)을 spike-findings에 적는다. 넘으면 그 사실을 README 알려진 한계의 레티나 줄에 적는다 |
| 실제 결과 | 미수행 |

### P8. 2a 클립보드 시험을 텍스트 클립보드로 다시

| | |
|---|---|
| 목적 | 2b 동안 두 번째 기계의 클립보드에 텍스트가 아닌 내용(서식 있는 글)이 있어 클립보드 시험 15개(`e2e_clipboard` 14개, `e2e_mcp_isolation`의 `mcp_agent_cannot_write_the_clipboard` 1개)가 시작에서 거부되었다(제품 실패가 아니라 시험 도구의 보호). 2a에서는 통과했다 |
| 2b가 클립보드 코드를 바꿨나 | `git diff 2a12c0e..2be62cc`(2a의 마지막 커밋에서 2b 작업 7 끝까지)로 확인했다(2026-11-18): 클립보드 시험 파일(`crates/pegboard-core/tests/e2e_clipboard.rs`, `tests/shared/clipboard.rs`)과 `argv.rs`(키 조합 거부)는 바뀌지 않았다. 바뀐 것은 셋이고 모두 알림이나 공통 층이다: `guard.rs`가 클립보드 리스너를 넣고 페이지를 다시 불러오기 직전에 녹화기에 알리는 줄(`announce_reload`), `downloads.rs`의 클립보드 권한 거부·해제(`deny_clipboard`, `lift_clipboard`)가 탭을 활성화하기 직전에 녹화기에 알리는 줄(`on_activating`). 둘 다 그 일을 사람 개입으로 잘못 남기지 않으려는 것이고, 리스너, 다시 불러오기, 권한 거부 자체는 같다. 셋째는 모든 CDP 사용자가 지나는 `cdp.rs`의 응답 처리(답마다 그 전에 받은 이벤트 수를 세고, 버린 요청을 지운다). 그래서 "2b는 클립보드 코드를 바꾸지 않았다"는 방어 로직에 대해서는 맞고, 공통 CDP 층의 변경은 다시 돌려 확인한다 |
| 절차 | 1. 클립보드에 지키고 싶은 것이 있으면 따로 옮겨 둔다(다음 줄이 지운다). 2. `printf 'pegboard-2b' \| pbcopy` (일반 텍스트만 남긴다) 3. 저장소 루트에서 `cargo test -p pegboard-core --test e2e_clipboard -- --ignored --test-threads=1` 4. `cargo test -p pegboard-cli --test e2e_mcp_isolation mcp_agent_cannot_write_the_clipboard -- --ignored` 5. `pbpaste` |
| 기대 결과 | 3, 4 모두 통과("refusing to run: the system clipboard holds non-text data" 같은 시작 거부가 없다). 5가 `pegboard-2b`(시험이 클립보드를 되돌렸다) |
| 실제 결과 | 미수행 |
