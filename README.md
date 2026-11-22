# usAgI

AI 구독 사용량(5시간 / 주간 한도)을 항상 보여 주는 가벼운 트래커.

- Windows: 작업 표시줄 위젯 + 알림 영역 아이콘
- macOS: 메뉴 막대 표시
- Rust 단일 실행 파일 (Windows exe 약 2.5MB), 메모리 약 45MB 소모
- 공식 CLI가 알려 주는 값만 사용 (추정 없음, 토큰·비밀번호 저장 없음)

```
[Claude] 5시간 ▬▬▬▭▭ 58%  Fable ▬▭▭▭▭ 3%  │  [ChatGPT] 주간 ▬▬▬▬▭ 92%      ^  오전 6:30
         주간  ▬▬▭▭▭ 38%
```

## 내려받기

[Releases](../../releases)에서 내려받기.

| 파일 | 용도 |
|---|---|
| `usAgI-portable-x.y.z.zip` | 포터블: 압축을 풀고 `usagi.exe` 실행. 설정은 옆의 `data` 폴더에 저장 |
| `usagi.exe` | 실행 파일만 (설정은 `%APPDATA%\usAgI`) |

필요한 것: Windows 10/11, WebView2 런타임(Windows 11 기본 포함), 공식 Claude Code / Codex CLI.

### macOS

메뉴 막대에 공급자 로고와 사용량 표시. 개발자 인증서 없이 배포하지만 아래 방법은
"확인되지 않은 개발자" 경고 없이 설치 가능.

```bash
# 한 줄 설치 (업데이트도 같은 명령)
curl -fsSL https://raw.githubusercontent.com/Ba-koD/usAgI/main/scripts/install-mac.sh | bash

# 또는 Homebrew
brew install --cask Ba-koD/usagi/usagi
```

브라우저로 `usAgI-mac-x.y.z.zip`을 직접 받은 경우: 처음 한 번은 앱 우클릭 → **열기**로 실행.

## 사용법

| 동작 | 결과 |
|---|---|
| 위젯 더블클릭 | 설정 창 |
| 위젯 우클릭 | 퍼센트 ↔ 리셋까지 남은 시간 |
| 알림 영역 아이콘 우클릭 | 설정 열기 · 위젯 표시 · 새로고침 · 종료 |
| `usagi.exe` 한 번 더 실행 | 이미 떠 있으면 설정 창만 열림 |

- 작업 표시줄 자리가 모자라면 남는 공간에 맞춰 축소: 막대 → 라벨+값 → `5h 57% · 7d 38%` → 숨김
- 처음엔 연결된 계정 없음. 사용량 화면의 **+ 추가** → Claude / ChatGPT 공식 로그인으로 추가
- 알림도 처음엔 없음. 그래프마다 **+ 알림 추가**로 기준 설정, 한도 도달·리셋과 로그인 만료 알림은 알림 화면에서 켜기
- 계정 카드 왼쪽의 잡는 표시(⠿)를 끌어서 순서 변경 (위젯·메뉴 막대도 같은 순서)
- Claude 계정은 실시간 연결 자동: Claude Code 상태 표시줄에 브리지를 걸어 메시지마다 갱신 (`settings.json` 백업 후 수정, 기존 상태 표시줄 출력은 유지, 계정 삭제 시 원복)

## 사용량 출처

추정 없음, 토큰·비밀번호 읽기·저장 없음. 공식 CLI가 알려 주는 값만 사용.

| 공급자 | 출처 | 비용 |
|---|---|---|
| Claude | Claude Code의 `/usage` 명령 (`claude -p "/usage"`, Claude Code 안에서 처리) + 상태 표시줄 브리지(사용 중 실시간) | 토큰 0, 5시간 창을 열지 않음 |
| ChatGPT (Codex) | `codex app-server`의 `account/rateLimits/read`, `account/read` | 토큰 0 |

요금제 표시는 공식 정보 그대로: Claude는 Claude Code 프로필의 `organizationRateLimitTier`(예: Max 5x),
ChatGPT는 Codex의 요금제 ID(prolite/pro/promax → Pro 100/200/500).

로그인은 항상 공식 `claude auth login` / `codex login` 사용. Anthropic 정책상 서드파티 앱은
Claude.ai 로그인 대행과 자격 증명 저장 불가
([legal-and-compliance](https://code.claude.com/docs/en/legal-and-compliance)).

## 업데이트

설정 → 정보에서 **업데이트 확인**, **자동 업데이트**(기본 켜짐) 선택.

1. GitHub 최신 릴리스 확인
2. 새 버전이면 이 저장소의 릴리스 파일 내려받기 (Windows `usagi.exe`, macOS `usAgI-mac.zip`)
3. 같은 릴리스의 `.sha256` 체크섬으로 검증 (없거나 다르면 미적용)
4. 실행 파일 교체 후 다시 시작 (설정 창이 열려 있으면 닫을 때까지 대기)

## 보안

- 설정 창: 앱에 들어 있는 화면만 표시. 다른 주소로 이동·새 창 열기 차단, 들어 있는 스크립트만 실행하고 외부 통신을 막는 보안 정책(CSP) 적용
- 업데이트: 이 저장소의 GitHub 릴리스 파일만 허용, 체크섬 필수
- 계정 삭제: usAgI가 만든 프로필 폴더(프로필 폴더 바로 아래 한 단계)만 삭제
- 로그인·토큰: 공식 CLI가 처리, usAgI는 토큰 읽기·저장 없음

## 빌드

```powershell
cargo test -p usagi-core
cargo build --release -p usagi-win        # target\release\usagi.exe
scripts\package-portable.ps1              # dist\usAgI-portable-x.y.z.zip
```

`USAGI_UPDATE_REPO=owner/name`을 지정하고 빌드하면 그 저장소에서 업데이트 수신 (릴리스 빌드는 자동 지정).

## 릴리스 만들기

1. `CHANGELOG.md` 맨 위에 `## [x.y.z] - 날짜` 섹션 작성
2. `scripts\release.ps1 -Version x.y.z -Tag` (버전 올리기 → 테스트·빌드 → 커밋·태그, push는 안 함)
3. `git push origin main` 후 `git push origin vx.y.z`
4. GitHub Actions(`.github/workflows/release.yml`) 자동 실행:
   - **준비** (몇 초): 태그와 `Cargo.toml` 버전 일치, `CHANGELOG.md` 섹션 확인, 릴리스 노트 추출
   - **빌드** (Windows·macOS 동시): 각 OS에서 공통 로직 테스트 → `scripts/package-portable.ps1` / `scripts/package-mac.sh`가 `dist/release/`에 파일 생성
   - **게시**: 초안 릴리스에 모든 파일과 `SHA256SUMS.txt` 업로드 후 한 번에 공개, Homebrew 탭 갱신(`HOMEBREW_TAP_TOKEN` 비밀값이 있을 때)

- main 푸시: 같은 준비 → 빌드 실행 (게시 없음). 산출물은 Actions 실행 페이지에서 7일간 내려받기 가능, 이때 저장한 캐시를 태그 빌드가 재사용
- PR: `ci.yml`이 세 OS에서 테스트와 컴파일 확인만 실행
- 수동: Actions → Build & Release → **Run workflow** (기본은 빌드만)
- 로컬에서 같은 파일 만들기: `scripts\package-portable.ps1`(Windows), `bash scripts/package-mac.sh`(macOS)

| 릴리스 파일 | 쓰는 곳 |
|---|---|
| `usagi.exe`, `usagi.exe.sha256` | Windows 자동 업데이트 |
| `usAgI-portable-x.y.z.zip` | Windows 포터블 내려받기 |
| `usAgI-mac.zip`, `usAgI-mac.zip.sha256` | macOS 설치 스크립트·자동 업데이트 (latest/download 고정 이름) |
| `usAgI-mac-x.y.z.zip` | Homebrew Cask |
| `SHA256SUMS.txt` | 모든 파일 체크섬 |

## 구조

```
crates/usagi-core   공통 로직 (사용량 읽기, 계정, 알림 판단, 문구)
                    + settings.html: 모든 플랫폼 공용 설정 화면
                    + 설정 화면 컨트롤러(controller.rs): 화면 상태와 모든 명령 처리
apps/usagi-win      Windows 앱 (작업 표시줄 위젯, 알림 영역, 업데이트 적용)
apps/usagi-mac      macOS 앱 (메뉴 막대 로고 + 사용량, 계정별 메뉴, 업데이트 적용)
scripts             포터블 패키지 · macOS 앱 묶기 · 설치 · 릴리스 스크립트
packaging/homebrew  Homebrew Cask 틀 (릴리스 때 탭에 자동 반영)
```

- 설정 화면: 모든 플랫폼이 같은 `settings.html` 사용
- 명령 처리: 화면에서 온 명령은 모두 `usagi-core`의 `Controller`가 처리
- 플랫폼 앱: 컨트롤러가 돌려준 일(`Effect`: 주소 열기, 알림, 시작 시 실행, 위젯·메뉴 막대 다시 그리기, 업데이트 받기·적용)만 실행
- Linux 추가 시 플랫폼 앱 부분만 구현
