# usAgI 개발 문서

사용량을 어디서 받는지, 업데이트와 보안 처리, 빌드와 릴리스 방법, 코드 구조.
프로그램 소개와 설치는 [README](../README.md) 참고.

## 사용량 출처

공급자가 제공하는 수치를 그대로 표시.

| 공급자 | 출처 | 비용 |
|---|---|---|
| Claude | Claude Code의 `/usage` 명령 (`claude -p "/usage"`) + 상태 표시줄 연결(사용 중 실시간) | 토큰 0, 5시간 창을 열지 않음 |
| ChatGPT (Codex) | `codex app-server`의 `account/rateLimits/read`, `account/read` | 토큰 0 |

- 요금제 표시: Claude는 Claude Code 프로필의 `organizationRateLimitTier`(예: Max 5x), ChatGPT는 Codex의 요금제 ID(prolite/pro/promax → Pro 100/200/500)
- 로그인: 공식 `claude auth login`과 `codex login`이 처리. 세션이 만료되면 계정 카드의 **다시 로그인**으로 다시 연결
- 브라우저 쿠키와 공급자 웹 API는 사용하지 않음

## 업데이트

설정 → 정보에서 **업데이트 확인**, **자동 업데이트**(기본 켜짐) 선택.

1. GitHub 최신 릴리스 확인
2. 새 버전이면 이 저장소의 릴리스 파일 내려받기 (Windows `usagi.exe`, macOS `usAgI-mac.zip`)
3. 같은 릴리스의 `.sha256` 체크섬 확인
4. 서명(`.minisig`) 확인: 앱에 들어 있는 공개 키(`packaging/usagi-update.pub`)로 검증. 서명 안의 버전과 파일 이름도 일치해야 함
5. 실행 파일을 바꾸고 다시 시작 (설정 창이 열려 있으면 닫을 때까지 대기)

체크섬이나 서명이 없거나 맞지 않으면 설치하지 않음. 서명 비밀 키는 저장소에 두지 않고 GitHub Actions 비밀값으로만 보관하므로, 릴리스 파일만 바꿔서는 통과할 수 없음.
(공개 키 파일이 비어 있는 빌드는 체크섬만 확인.)

내려받은 파일을 직접 확인하려면 [minisign](https://jedisct1.github.io/minisign/) 사용:

```bash
minisign -Vm usagi.exe -p packaging/usagi-update.pub
```

## 보안

- **로그인·토큰**: 공식 CLI가 관리. usAgI는 읽거나 저장하거나 전송하지 않음
- **프로필 폴더**: usAgI가 계정마다 만드는 폴더는 이 사용자만 접근 (Windows는 상속 권한 차단, macOS는 0700)
- **네트워크**: GitHub 업데이트 확인·내려받기와 공식 CLI 설치 스크립트 내려받기뿐. 사용 정보 수집 없음
- **업데이트**: 이 저장소의 GitHub 릴리스 파일만 허용, 체크섬과 서명 필수
- **CLI 자동 설치**: 각 회사의 공식 설치 스크립트만 실행 (`https://claude.ai/install.*`, `https://chatgpt.com/codex/install.*`)
- **설정 창**: 실행 파일에 들어 있는 Slint 화면 (웹 페이지·스크립트·로컬 포트 없음)
- **계정 삭제**: usAgI가 만든 프로필 폴더만 삭제

## 빌드

```powershell
cargo test -p usagi-core
cargo build --release -p usagi-win        # target\release\usagi.exe
scripts\package-portable.ps1              # dist\usAgI-portable-x.y.z.zip
```

macOS는 `bash scripts/package-mac.sh`.

`USAGI_UPDATE_REPO=owner/name`을 지정하고 빌드하면 그 저장소에서 업데이트 수신 (릴리스 빌드는 자동 지정).

## 릴리스 만들기

### 처음 한 번: 서명 키 만들기

```powershell
cargo run -p usagi-sign -- keygen
Get-Content "$env:USERPROFILE\.usagi\update-signing.key" -Raw | gh secret set UPDATE_SIGNING_KEY
```

- 비밀 키: `%USERPROFILE%\.usagi\update-signing.key` (저장소 밖). 안전한 곳에 백업
- 공개 키: `packaging/usagi-update.pub`에 기록. 이 파일을 커밋하면 다음 빌드부터 서명 없는 업데이트를 받지 않음
- GitHub Actions 비밀값 `UPDATE_SIGNING_KEY`: 릴리스 때 서명에 사용. 키에 비밀번호를 걸었으면 `UPDATE_SIGNING_PASSWORD`도 등록
- 키를 바꾸면 예전 키가 들어 있는 설치본은 새 버전을 받지 못하므로 바꾸지 않는 것이 원칙

### 릴리스마다

1. `CHANGELOG.md` 맨 위에 `## [x.y.z] - 날짜` 섹션 작성
2. `scripts\release.ps1 -Version x.y.z -Tag` (버전 올리기 → 테스트·빌드 → 버전 파일 커밋·태그, push는 하지 않음)
3. `git push origin main` 후 `git push origin vx.y.z`
4. GitHub Actions(`.github/workflows/release.yml`)가 자동 실행
   - **준비**: 태그와 `Cargo.toml` 버전 일치, `CHANGELOG.md` 섹션 확인, 릴리스 노트 추출
   - **빌드** (Windows·macOS 동시): 테스트 후 `scripts/package-portable.ps1` / `scripts/package-mac.sh`로 파일 생성
   - **게시**: 파일마다 서명(`.minisig`) 생성 → 모든 파일과 `SHA256SUMS.txt`를 올린 뒤 공개, Homebrew 탭 갱신(`HOMEBREW_TAP_TOKEN` 비밀값이 있을 때)

공개 키가 저장소에 있는데 `UPDATE_SIGNING_KEY` 비밀값이 없으면 게시 단계가 실패 (서명 없는 릴리스는 설치된 앱이 받지 않음).
이미 게시된 릴리스에 서명을 다시 올릴 때는 `scripts\sign-release.ps1 -Version x.y.z`.

- main 푸시와 PR: `ci.yml`이 세 OS에서 테스트와 컴파일 확인만 실행 (2~3분). 릴리스 빌드는 태그에서 한 번만 실행
- 수동: Actions → Build & Release → **Run workflow** (기본은 빌드만. 릴리스 전에 묶기까지 미리 확인할 때 사용, 산출물은 실행 페이지에서 7일간 내려받기 가능)

| 릴리스 파일 | 쓰는 곳 |
|---|---|
| `usagi.exe`, `.sha256`, `.minisig` | Windows 자동 업데이트 |
| `usAgI-mac.zip`, `.sha256`, `.minisig` | macOS 설치 스크립트·자동 업데이트 (latest/download 고정 이름) |
| `usAgI-portable-x.y.z.zip`, `.minisig` | Windows 포터블 내려받기 |
| `usAgI-mac-x.y.z.zip`, `.minisig` | Homebrew Cask |
| `SHA256SUMS.txt` | 모든 파일 체크섬 |

## 구조

```
crates/usagi-core       공통 로직 (사용량 읽기, 계정, 알림 판단, 문구, 업데이트 서명 확인)
                        + 설정 화면 컨트롤러(controller.rs): 화면 상태와 모든 명령 처리
crates/usagi-settings   Windows·macOS 공용 Slint 설정 화면과 호스트 통신
apps/usagi-win          Windows 앱 (작업 표시줄 위젯, 알림 영역, 업데이트 적용)
apps/usagi-mac          macOS 앱 (메뉴 막대 로고 + 사용량, 계정별 메뉴, 업데이트 적용)
tools/usagi-sign        릴리스 서명 도구 (키 만들기, 서명, 확인)
scripts                 포터블 패키지 · macOS 앱 묶기 · 설치 · 릴리스 · 서명 스크립트
packaging               업데이트 서명 공개 키, Homebrew Cask 틀
```

- 설정 화면: `crates/usagi-settings/ui/settings.slint`와 공용 컴포넌트
- 명령 처리: 화면에서 온 명령은 모두 `usagi-core`의 `Controller`가 처리
- 플랫폼 앱: 컨트롤러가 돌려준 일(`Effect`: 주소 열기, 알림, 시작 시 실행, 위젯·메뉴 막대 다시 그리기, 업데이트 받기·적용)만 실행
- Linux 추가 시 플랫폼 앱 부분만 구현
