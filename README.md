<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/logo-dark.png">
  <img src="docs/images/logo-light.png" width="80" alt="usAgI 로고">
</picture>

# usAgI

**Claude Code와 Codex의 구독 한도를 Windows 작업 표시줄과 macOS 메뉴 막대에서 확인**

[**최신 릴리스**](https://github.com/Ba-koD/usAgI/releases/latest) · [Windows 다운로드](https://github.com/Ba-koD/usAgI/releases/latest/download/usagi.exe) · [macOS 다운로드](https://github.com/Ba-koD/usAgI/releases/latest/download/usAgI-mac.zip) · [변경 내역](CHANGELOG.md)

Windows 10/11 · macOS 11 이상 (Apple Silicon·Intel)

<img src="docs/images/widget.png" alt="Claude와 Codex의 사용량을 표시한 작업 표시줄 위젯" width="840">

</div>

## 설치

최신 버전은 [GitHub Releases](https://github.com/Ba-koD/usAgI/releases/latest)에서 받을 수 있습니다.

설치한 뒤 업데이트는 앱에 맡기세요(**설정 → 정보**, 자동 업데이트 기본 켜짐). 앱은 내장된 공개 키로 업데이트의 Minisign 서명을 늘 확인합니다. 아래 설치 스크립트는 [minisign](https://jedisct1.github.io/minisign/)이 설치되어 있을 때만 서명을 확인하고, 없으면 같은 릴리스의 SHA-256만 확인합니다(처음 받은 파일을 그대로 믿는 설치). 그래서 업데이트할 때 스크립트를 다시 실행하는 것보다 앱 업데이트가 안전합니다.

### Windows

릴리스의 `usagi.exe`를 받아 실행하세요. PowerShell로 설치할 수도 있습니다.

```powershell
$installer = Join-Path $env:TEMP 'usagi-install.ps1'
$url = 'https://raw.githubusercontent.com/Ba-koD/usAgI/main/scripts/install-windows.ps1'
Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $installer
if ($?) { powershell.exe -NoProfile -ExecutionPolicy Bypass -File $installer }
```

### macOS

**처음 설치는 터미널 설치를 권장합니다.** 아래 명령으로 최신 버전을 설치하세요. 이후 업데이트는 앱이 합니다. ZIP으로 설치하려면 릴리스에서 `usAgI-mac.zip`을 받으세요.

```sh
curl -fsSL https://raw.githubusercontent.com/Ba-koD/usAgI/main/scripts/install-mac.sh | bash
```

## 주요 기능

설정의 **사용량 → 추가**에서 공급자를 고른 뒤 브라우저에서 연결할 계정으로 로그인합니다. 이미 로그인한 CLI 계정을 가져오려면 **기존 로그인 우선**을 선택합니다. CLI가 필요하면 앱에서 설치를 안내합니다.

- **사용량 확인** — 5시간·주간·모델별 한도, 리셋 시각, 제공되는 크레딧, 리필권, Claude 쿠폰과 잔액·자동 충전 상태를 확인합니다.
- **위젯·메뉴 막대 설정** — 표시할 계정과 그래프를 고릅니다. Windows 위젯이나 macOS 메뉴 막대 항목을 우클릭하면 퍼센트와 리셋까지 남은 시간을 전환합니다. **모양**에서 바뀐 표시를 미리 볼 수 있습니다.
- **알림** — 사용량 기준, 리셋 전 시점, 한도 도달·리셋, 리필권·쿠폰 만료를 설정합니다. 만료일을 아는 로그인은 3일 전부터 계정당 한 번 알립니다.

## 리소스 사용량

설정 창은 열 때 별도 프로세스로 실행됩니다. 창을 닫으면 설정 화면에 사용한 메모리가 반환됩니다.

| 측정 환경 | 다운로드 크기¹ | 앱 메모리 부담² | 전체 상주 메모리³ | 대기 CPU 시간⁴ |
| --- | ---: | ---: | ---: | ---: |
| Windows 11 · 작업 표시줄 위젯 | **약 9.9 MB** | **4.27 MB** | 29.00 MB | 94 ms/분 |
| macOS · 메뉴 막대 | **약 15.3 MB** | **15.43 MB** | 78.28 MB | 23.82 ms/분 |

¹ **작을수록 다운로드 부담이 적습니다.** 0.4.2 릴리스 첨부 파일 기준입니다. Windows는 `usagi.exe`, macOS는 Apple Silicon과 Intel을 모두 포함한 `usAgI-mac.zip`입니다.

² **작을수록 앱의 메모리 부담이 적습니다.** Windows는 **전용 작업 집합**, macOS는 **Physical Footprint**입니다.

³ **작을수록 상주량이 적습니다.** 공유 라이브러리와 파일 매핑을 포함한 Windows **전체 작업 집합**과 macOS **RSS**입니다. 앱 자체의 메모리 부담은 ²를 참고하세요.

⁴ **같은 기기와 조건에서 작을수록 좋습니다.** 본체 프로세스가 사용한 CPU 시간을 1분 기준으로 환산했습니다.

대기 수치는 0.4.2 릴리스 파일로 측정했습니다. 설정 창을 닫고 위젯 또는 메뉴 막대를 켠 상태에서, 같은 테스트 계정 2개와 로그 3,001개로 130초씩 두 번 측정한 평균입니다. 정기 사용량 조회와 업데이트 확인은 대기시켰으며, 메모리는 1초 간격 샘플의 중앙값입니다.

## 사용량 갱신과 업데이트

사용 가능한 로컬 로그인이 있으면 공급자의 사용량을 HTTPS로 조회합니다. 기본 갱신 주기는 5분이고, 필요할 때 공식 CLI로 조회합니다. 로그인과 토큰 갱신은 공식 CLI가 관리합니다.

앱 업데이트는 **설정 → 정보**에서 확인합니다. GitHub 릴리스의 SHA-256과 Minisign 서명을 검증한 뒤 설치하며, 설치 스크립트를 다시 실행하는 것보다 이 방법을 권장합니다. 검증에 실패한 버전은 바로 다시 받지 않고 간격을 늘려 다시 시도합니다. 최신 릴리스가 0.3.1 이상일 때 0.2.1 이하를 쓰고 있다면 앱 파일을 내려받아 한 번 업데이트하세요.

## 도움말

[문제 제보·기능 제안](https://github.com/Ba-koD/usAgI/issues) · [개발 문서](docs/DEVELOPMENT.md) · [릴리스](https://github.com/Ba-koD/usAgI/releases)

문제를 제보할 때는 usAgI 버전, OS와 재현 방법을 함께 적어 주세요.
