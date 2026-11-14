# 2a단계 수동 확인 (N1~N4)

2a 자동 시험이 덮지 못하는 것, 사람의 손이나 실제 MCP 클라이언트가 필요한 것이다. 아직 수행하지 않았다. 수행하면 "실제 결과" 칸을 채우고, 기대와 다르면 README의 "알려진 한계"와 [architecture.md](./architecture.md) 11절 "남는 위험"에 한 줄씩 더한다. 준비와 정리는 [phase1-manual-checks.md](./phase1-manual-checks.md)의 "준비"와 같다(시험용 `PEGBOARD_HOME`, 시험 페이지 서버). 클립보드를 쓰는 항목(N1, N2)은 시작 전에 시스템 클립보드의 내용을 따로 저장해 두고 끝나면 되돌린다.

## 한눈에 보기

| # | 확인하는 것 | 화면 조작 |
|---|---|---|
| N1 | 오른쪽 클릭 메뉴의 복사와 붙여넣기 | 에이전트 창의 탭에서 메뉴 고르기 |
| N2 | 사람이 에이전트 창의 탭에서 직접 누르는 복사·붙여넣기 키 | 키 누르기 |
| N3 | 정책을 다시 걸 때 에이전트 창이 사람의 키보드 초점을 가져가는지 | 다른 앱에서 타이핑 |
| N4 | 실제 MCP 클라이언트에서 쓰기 | 클라이언트 설정 |

준비에 더할 시험 페이지(글상자 하나):

```bash
cat > /tmp/pegboard-manual-site/clip.html <<'HTML'
<!doctype html><meta charset="utf-8"><title>clip</title>
<textarea id="t">pegboard clip check</textarea>
HTML
```

## 항목

### N1. 오른쪽 클릭 메뉴의 복사와 붙여넣기

| | |
|---|---|
| 목적 | 클립보드를 거부하는 세션(기본)의 탭에서 Chrome의 문맥 메뉴로 하는 복사와 붙여넣기가 막히는지. 자동 시험은 페이지 스크립트, 편집 명령 키, `navigator.clipboard`만 잰다 |
| 절차 | 1. `export PEGBOARD_SESSION="$(pegboard session create --name n1 --url http://127.0.0.1:8765/clip.html \| head -1)"` 2. 시스템 클립보드에 표식을 넣는다: `echo N1-MARKER \| pbcopy` 3. 에이전트 창의 그 탭에서 글상자의 글자를 마우스로 고르고 오른쪽 클릭 → "복사" 4. `pbpaste` 5. 글상자를 오른쪽 클릭 → "붙여넣기" 6. `pegboard get value '#t'` |
| 기대 결과 | 4에서 `N1-MARKER`가 그대로 나온다(복사가 막힘). 6의 값에 `N1-MARKER`가 들어 있지 않다(붙여넣기가 막힘). 막히지 않으면 알려진 한계로 적는다 |
| 실제 결과 | 미수행 |

### N2. 사람이 에이전트 창에서 직접 누르는 키

| | |
|---|---|
| 목적 | 사람이 에이전트 창의 탭에서 Command+C, Command+V를 직접 누를 때. 가드의 리스너는 사람의 복사도 막을 것으로 본다(README) |
| 절차 | N1의 세션에서: 1. `echo N2-MARKER \| pbcopy` 2. 에이전트 창의 글상자에서 글자를 고르고 Command+C 3. `pbpaste` 4. 글상자를 누르고 Command+V 5. `pegboard get value '#t'` 6. 같은 것을 `--allow clipboard`로 만든 세션(다른 프로필: `pegboard profile create n2; pegboard session create --profile n2 --allow clipboard --url http://127.0.0.1:8765/clip.html`)에서 한다 |
| 기대 결과 | 거부하는 세션: 3에서 `N2-MARKER` 그대로, 5에 `N2-MARKER` 없음. 허용하는 세션: 복사와 붙여넣기가 된다. 결과를 그대로 적는다(사람의 복사가 막히는 것은 알려진 동작이다) |
| 실제 결과 | 미수행 |

### N3. 정책을 다시 걸 때의 초점

| | |
|---|---|
| 목적 | 열린 프로필의 클립보드 설정이 바뀔 때 데몬은 그 프로필이 기본 컨텍스트가 아니면 그 프로필의 탭 하나를 활성화한다(README). 이때 에이전트 창이 앞으로 나와 사람이 다른 앱에서 치던 키를 가져가는지 |
| 절차 | 1. 사람 창(에이전트 창이 아닌 쪽)을 한 번 클릭해 사람 프로필을 마지막 활성으로 만든다. 2. 텍스트 편집기를 맨 앞에 두고 계속 입력한다. 3. 다른 터미널에서 [phase1-manual-checks.md](./phase1-manual-checks.md) M8b의 맨 앞 앱 기록 명령을 띄운다. 4. 또 다른 터미널에서: `pegboard profile create n3; s=$(pegboard session create --profile n3 \| head -1); pegboard session close "$s"; s=$(pegboard session create --profile n3 --allow clipboard \| head -1)` 5. 입력을 멈추고 기록을 멈춘다. 6. 텍스트 편집기의 글과 맨 앞 앱 기록을 본다 |
| 기대 결과 | 문서에 적힌 대로 에이전트 창이 앞으로 나올 수 있다. 나왔는지, 나왔다면 입력하던 글자가 에이전트 창으로 갔는지(편집기의 글이 끊겼는지)를 적는다. 글자가 에이전트 창의 페이지에 들어갔다면 그 사실을 architecture.md 11절에 적는다 |
| 실제 결과 | 미수행 |

### N4. 실제 MCP 클라이언트에서 쓰기

| | |
|---|---|
| 목적 | README "빠른 시작"의 MCP 설정이 실제 클라이언트에서 동작하는지 |
| 절차 | 1. 쓰는 MCP 클라이언트(예: Claude Code)의 설정에 README의 예(`"args": ["mcp", "--allow-domain", "example.com"]`)를 넣는다. `PEGBOARD_HOME`을 시험용으로 주려면 설정의 `env`에 넣는다. 2. 에이전트에게 "example.com을 열고 제목을 알려 줘"를 시킨다. 3. 에이전트에게 "https://www.iana.org 를 열어 봐"를 시킨다. 4. 클라이언트를 끈다. 5. `pegboard session list`와 `pegboard recording list` |
| 기대 결과 | 2는 된다(세션이 에이전트 창에 탭그룹으로 생긴다). 3은 `policy_denied`로 거부된다(허용목록 밖). 4 뒤에 5의 세션 목록에서 그 세션이 사라지고 기록은 남아 마감돼 있다. 도구 결과 어디에도 `<id>.<토큰>` 꼴이 보이지 않는다 |
| 실제 결과 | 미수행 |
