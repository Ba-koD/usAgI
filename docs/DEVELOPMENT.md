# usAgI 개발 문서

사용량을 어디서 받는지, 업데이트와 보안 처리, 빌드와 릴리스 방법, 코드 구조.
프로그램 소개와 설치는 [README](../README.md) 참고.

## 사용량 출처

공급자가 제공하는 수치를 그대로 표시.

| 공급자 | 출처 | 비용 |
|---|---|---|
| Claude | 파일 로그인으로 HTTPS 사용량 조회 또는 Claude Code의 `/usage`, 상태 표시줄 연결(사용 중 실시간) | 토큰 0, 5시간 창을 열지 않음 |
| ChatGPT (Codex) | 파일 로그인으로 HTTPS 사용량 조회 또는 `codex app-server`의 `account/rateLimits/read`, `account/read` | 토큰 0 |

- 요금제 표시: Claude는 Claude Code 프로필의 `organizationRateLimitTier`(예: Max 5x), ChatGPT는 Codex의 요금제 ID(prolite/pro/promax → Pro 100/200/500)
- 로그인: 공식 `claude auth login`과 `codex login`이 처리. 세션이 만료되면 계정 카드의 **다시 로그인**으로 다시 연결
- Codex 파일 로그인은 `chatgpt.com`의 Codex 백엔드에서 한 번에 사용량 조회. 사용 가능한 리필권의 만료 정보가 빠져 있으면 세부 정보도 읽음
- Claude Code 파일 로그인은 `user:profile` 권한이 있는 액세스 토큰으로 `api.anthropic.com`의 OAuth 사용량 응답을 한 번 읽음
- 유효한 Claude 파일 세션이 있으면 시작과 설정창 열기 시 로그인 상태를 로컬에서 확인. 서버가 세션을 거부하면 CLI로 전환하고, 요청 제한 응답의 대기 시간 동안 사용량 갱신을 보류
- OS 보안 저장소, 만료된 파일 로그인, 별도 백엔드 설정은 CLI 경로로 처리
- HTTP 429는 계정별로 대기 시각을 저장. `Retry-After`를 최대 1시간까지 적용하고 유효한 값이 없으면 5분 대기
- Codex 대체 프로필은 이메일과 ChatGPT 워크스페이스 ID가 모두 일치할 때 사용
- Codex에서 기록된 연결 시각이 있으면 그 이후 시작된 세션의 로그만 사용. 세션 헤더로 시작 시각을 확인하고 현재 로그가 바뀌면 전체 탐색 사이에 그 파일만 다시 읽음
- Claude 대체 프로필은 계정·조직을 조회 전후에 확인. 파일 로그인 경로는 확인한 액세스 토큰으로 요청하고, 대체 폴더의 로컬 기록은 합치지 않음

## 업데이트

설정 → 정보에서 **업데이트 확인**, **자동 업데이트**(기본 켜짐) 선택.

1. GitHub 최신 릴리스 확인
2. 새 버전이면 이 저장소의 릴리스 파일 내려받기 (Windows `usagi.exe`, macOS `usAgI-mac.zip`)
3. 릴리스 본문의 **파일 검증 정보**에서 SHA-256과 Minisign 서명 읽기. 이전 릴리스의 검증 정보는 첨부에서 읽음
4. 체크섬과 서명을 검증. 서명은 앱에 들어 있는 공개 키(`packaging/usagi-update.pub`)로 확인하며 서명 안의 버전과 파일 이름도 일치해야 함
5. 실행 파일을 바꾸고 다시 시작 (설정 창이 열려 있으면 닫을 때까지 대기)

확인과 내려받기는 Windows·macOS가 같은 코드(`usagi-core`의 `update`)로 하고, 실행 파일 바꾸기만 플랫폼마다 다름.
처음 주소는 GitHub의 https만 허용하고, GitHub이 파일 서버로 넘기는 리다이렉트도 https만 최대 5번 따라감.

체크섬이나 서명이 없거나 맞지 않으면 설치하지 않음. 서명 비밀 키는 저장소에 두지 않고 GitHub Actions 비밀값으로만 보관하므로, 릴리스 파일만 바꿔서는 통과할 수 없음.
(공개 키 파일이 비어 있는 빌드는 체크섬만 확인.)

내려받은 파일을 직접 확인하려면 본문의 서명을 `usagi.exe.minisig`에 저장하고 [minisign](https://jedisct1.github.io/minisign/) 사용:

```bash
minisign -Vm usagi.exe -p packaging/usagi-update.pub
```

## 보안

- **로그인·토큰**: 공식 CLI가 관리. 직접 조회는 액세스 토큰을 메모리에서 읽어 해당 제공자의 고정 HTTPS 주소로 보냄. 토큰 파일 쓰기와 갱신은 CLI가 담당
- **프로필 폴더**: usAgI가 계정마다 만드는 폴더는 이 사용자만 접근 (Windows는 상속 권한 차단, macOS는 0700)
- **네트워크**: Claude·Codex 사용량 조회, GitHub 업데이트 확인·내려받기
- **프록시**: 모든 HTTPS 요청이 OS 설정을 따름 (`usagi-core`의 `net`). 환경 변수 `HTTPS_PROXY`·`ALL_PROXY`(`NO_PROXY` 포함)가 있으면 우선, 없으면 Windows 인터넷 옵션(수동 프록시·예외 목록·PAC 주소·자동 검색), macOS 네트워크 설정(수동 프록시·예외 목록·PAC). 루프백 주소는 늘 직접. 프록시 조회 결과는 주소마다 10분 기억
  - `http://` 프록시만 씀. `https://` 프록시는 목적지와의 TLS가 생략돼 토큰이 프록시에 평문으로 가므로 쓰지 않고, SOCKS도 지원하지 않음. 이런 설정만 있으면 직접 연결로 우회하지 않고 요청을 보내지 않음
  - PAC가 여러 경로를 주면 연결이 안 될 때 다음 경로로 (앞에서 최대 4개). PAC에는 `스킴://호스트/`만 넘김
  - PAC 파일 서버가 로그인을 요구해도 Windows 자격 증명으로 다시 시도하지 않음 (자동 검색으로 찾은 PAC는 같은 네트워크의 누구나 낼 수 있음)
- **업데이트 받기 시간**: 연결 15초, 요청 하나의 전체 시간은 릴리스·검증 정보 60초, 새 버전 파일 10분 (느린 연결에서도 끝까지 받도록 본문까지 포함한 전체 시간으로 잼). macOS는 무작위 이름의 이 사용자 전용 임시 폴더에 받고 풂
- **업데이트**: 이 저장소의 GitHub 릴리스 파일만 허용, 체크섬과 서명 필수
- **CLI 준비**: 유효한 파일이나 설치된 CLI를 사용. CLI가 필요한 경우 사용자 선택으로 공급자의 공식 설치 문서를 엶
- **설정 창**: 별도 Slint 실행 파일을 열고 JSON 파이프로 통신. 계정·설정·알림은 본체가 읽고 저장
- **계정 제거**: usAgI 계정·사용량·관련 설정을 제거하고 Claude 상태 표시줄 설정 복원. 프로필·기록 삭제는 usAgI 전용 폴더에 한정
- 프로필과 캐시 삭제는 계정 목록과 알림 설정을 저장한 후 시작. 저장 실패 시 등록과 프로필 유지
- **신규 로그인 취소**: 등록 전 프로필은 대체 프로필로 사용하지 않으며, 취소하면 해당 폴더 정리. 남은 미등록 폴더는 설정의 **일반 → 보관된 프로필**에서 정리 가능

## 빌드

```powershell
cargo test -p usagi-core
cargo test -p usagi-core real_release -- --ignored  # 실제 GitHub 릴리스로 확인·받기·서명 검증 (네트워크)
cargo build --release -p usagi-win -p usagi-settings  # 개발용, 두 파일을 같은 폴더에서 실행
scripts\package-portable.ps1              # 로컬 포터블 묶음 dist\usAgI-portable-x.y.z.zip
```

Windows 배포용 `dist/release/usagi.exe`는 포터블 패키지 스크립트가 설정 실행 파일을 압축해 내장함. macOS는 `bash scripts/package-mac.sh`가 본체와 설정 실행 파일을 같은 앱에 넣고 `dist/release/usAgI-mac.zip`을 생성함.

빌드 프로필 (`Cargo.toml`)
- `release`: 로컬 개발용. thin LTO라 다시 빌드가 빠름
- `dist`: 배포용 본체. fat LTO + `codegen-units = 1`로 가장 작게 (패키지 스크립트·CI가 사용)
- `settings-release`: 설정 창. 크기 우선(`opt-level = "z"`), 코드는 16개로 나눠 병렬 생성하고 링크 때 fat LTO로 다시 최적화

테스트 빌드
- 포터블 사본(exe 옆 `data\usagi.portable`)은 데이터 폴더마다 따로 실행되므로, 설치형을 그대로 둔 채 옆에서 띄워 확인 가능
- `cargo build --release -p usagi-win --features mock`: 가짜 사용량만 보내는 화면 확인용 빌드 (CLI·네트워크·로그 읽기 없음, 자동 실행·알림 등록·업데이트는 건드리지 않음, 포터블 data 폴더가 있어야 실행)

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
2. `scripts\release.ps1 -Version x.y.z -Commit` (버전 올리기 → 테스트·컴파일 확인 → 버전 파일 커밋, push는 하지 않음)
3. `git push origin main` (태그는 올리지 않음. 게시할 때 CI가 그 커밋에 `vx.y.z` 태그 생성)
4. GitHub Actions(`.github/workflows/release.yml`)가 자동 실행
   - **준비**: 아직 게시하지 않은 버전이고 `CHANGELOG.md`에 그 섹션이 있을 때만 진행. 같은 이름의 태그가 다른 커밋에 있으면 중단. 릴리스 노트 추출
   - **빌드**: Windows 본체·Windows 설정 창·Mac 본체(ARM·Intel)·Mac 설정 창(ARM·Intel)·공통 테스트(Windows)·서명 도구를 준비 단계를 기다리지 않고 각자 다른 러너에서 동시에. 묶기 단계에서 Windows는 본체에 설정 창을 합치고, Mac은 유니버설 ZIP 생성. macOS 테스트는 같은 커밋의 `ci.yml`이 실행 (무료 요금제의 macOS 동시 작업 5개 제한 때문에 릴리스 쪽에서는 빼서 Mac 빌드가 줄을 서지 않게)
   - **게시**: 파일마다 서명(`.minisig`)을 만들고 릴리스 본문의 **파일 검증 정보** 접기 영역에 SHA-256과 서명을 넣음. 배포 파일을 올린 뒤 공개하고, `HOMEBREW_TAP_TOKEN` 비밀값이 있으면 Homebrew 탭 갱신

공개 키가 저장소에 있는데 `UPDATE_SIGNING_KEY` 비밀값이 없으면 게시 단계가 실패 (서명 없는 릴리스는 설치된 앱이 받지 않음).
이미 게시된 릴리스에 서명을 다시 올릴 때는 `scripts\sign-release.ps1 -Version x.y.z`. 본문의 체크섬(이전 릴리스는 `SHA256SUMS.txt`)으로 파일을 확인한 뒤 본문을 갱신. 검증 첨부가 있는 릴리스의 서명 첨부도 갱신. `-NoUpload`로 서명과 갱신할 본문을 로컬에서 확인 가능.

0.3.0은 기존 앱도 자동 업데이트할 수 있도록 `usagi.exe`와 `usAgI-mac.zip`의 `.sha256`·`.minisig`를 함께 첨부. 0.3.1부터 검증 정보는 본문에만 게시. 0.3.0을 지나친 0.2.1 이하 앱은 한 번 수동 업데이트 필요. macOS 설치 스크립트는 릴리스 본문에서 SHA-256을 읽고, 이전 릴리스는 첨부의 체크섬을 사용.

- main 푸시와 PR: `ci.yml`이 세 OS에서 테스트와 컴파일 확인만 실행 (2~3분). 릴리스 빌드는 버전을 올린 커밋에서 한 번만 실행
- 캐시: 릴리스 빌드가 main에서 돌아 의존성 캐시를 main에 저장하고, 다음 릴리스가 그 캐시를 받아 씀 (태그에서 만든 캐시는 다른 태그가 못 읽어서 예전 구조는 매번 처음부터 빌드하기 쉬웠음). 캐시를 미리 데우는 수동 실행은 필요 없음
- 캐시 저장: `scripts/ci-deps-hash.sh`의 의존성 지문(외부 의존성·기능·프로필·Rust 버전, 우리 버전 번호 제외)이 지난번과 같으면 큰 캐시를 다시 올리지 않음. 버전만 올린 릴리스는 작업마다 업로드 시간을 아낌
- `release.yml`은 `Cargo.toml`·`Cargo.lock`이 바뀐 main 푸시에서만 실행. 의존성만 바뀐 푸시는 빌드만 해서 캐시를 미리 새로 만듦 (게시 안 함)
- 테스트·컴파일 확인은 디버그 정보 없이(`CARGO_PROFILE_DEV_DEBUG=0`) 빌드해서 빠르고 캐시가 작음
- 수동 실행(Actions → Build & Release → **Run workflow**)은 빌드만 하고, 실행 페이지에서 산출물과 `timings-*` 컴파일 리포트를 7일간 확인 가능. main에서 **publish**를 켜면 아직 게시하지 않은 버전을 게시. **save_cache**를 켜면 main이 아닌 브랜치에도 의존성 캐시를 저장 (그 브랜치에서만 읽힘, 캐시 효과 확인용)

| 릴리스 파일 | 쓰는 곳 |
|---|---|
| `usagi.exe` | Windows 다운로드·자동 업데이트 |
| `usAgI-mac.zip` | Mac 직접 다운로드·Homebrew·설치 스크립트·자동 업데이트 |

## 구조

```
crates/usagi-core       공통 로직 (사용량 읽기, 계정, 알림 판단, 문구, 상태 표시줄 명령)
                        + 설정 화면 컨트롤러(controller.rs): 화면 상태와 모든 명령 처리
                        + 업데이트 확인·받기·서명 검증(update), OS 프록시를 따르는 HTTPS(net)
crates/usagi-settings   Windows·macOS 공용 Slint 설정 화면과 호스트 통신
apps/usagi-win          Windows 앱 (작업 표시줄 위젯, 알림 영역, 받은 exe 바꾸기)
apps/usagi-mac          macOS 앱 (메뉴 막대 로고 + 사용량 막대, 앱 자체 알림, 받은 .app 바꾸기)
tools/usagi-sign        릴리스 서명 도구 (키 만들기, 서명, 확인)
scripts                 포터블 패키지 · macOS 앱 묶기 · 설치 · 릴리스 · 서명 스크립트
packaging               업데이트 서명 공개 키, Homebrew Cask 틀
```

- 설정 화면: `crates/usagi-settings/ui/settings.slint`와 공용 컴포넌트
- 명령 처리: 화면에서 온 명령은 모두 `usagi-core`의 `Controller`가 처리
- 플랫폼 앱: 컨트롤러가 돌려준 일(`Effect`: 주소 열기, 알림, 시작 시 실행, 위젯·메뉴 막대 다시 그리기, 업데이트 받기·적용)만 실행
- Linux 추가 시 플랫폼 앱 부분만 구현
