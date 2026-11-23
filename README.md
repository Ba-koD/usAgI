<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/logo-dark.png">
  <img src="docs/images/logo-light.png" width="80" alt="usAgI 로고">
</picture>

# usAgI

**Claude와 ChatGPT 구독 사용량을 작업 표시줄에서 바로 확인**

[![릴리스](https://img.shields.io/github/v/release/Ba-koD/usAgI?label=release)](https://github.com/Ba-koD/usAgI/releases/latest)
[![빌드](https://github.com/Ba-koD/usAgI/actions/workflows/release.yml/badge.svg)](https://github.com/Ba-koD/usAgI/actions/workflows/release.yml)
![플랫폼](https://img.shields.io/badge/platform-Windows%20%7C%20macOS-4c8bf5)

[내려받기](https://github.com/Ba-koD/usAgI/releases/latest) · [기능](#기능) · [설치](#설치) · [작동 방식](#작동-방식) · [개발 문서](docs/DEVELOPMENT.md)

<img src="docs/images/widget.png" alt="작업 표시줄 위젯" width="840">

</div>

## 소개

Claude Code나 Codex를 쓰다가 한도가 얼마나 남았는지 확인하려면 `/usage`를 입력하거나 사용량 페이지를 열어야 함.
usAgI는 그 수치를 Windows 작업 표시줄과 macOS 메뉴 막대에 항상 표시하고, 정해 둔 기준에 도달하면 알림을 보내는 프로그램.

- 공급자가 알려 주는 수치를 그대로 표시 (추정 없음)
- 로그인과 토큰 갱신은 공식 CLI가 관리. Claude Code와 Codex의 파일 로그인은 usAgI가 사용량 조회에 재사용
- Rust로 만든 단일 실행 파일, 실행 중 메모리 약 8MB

## 기능

### 작업 표시줄 위젯

계정과 그래프별로 표시할 항목을 고르고, 퍼센트와 리셋까지 남은 시간을 우클릭으로 전환.
작업 표시줄 자리가 좁으면 막대 → 라벨과 값 → 짧은 표기 순서로 자동 축소.

<p align="center"><img src="docs/images/widget-compact.png" alt="좁은 자리에서 줄어든 위젯" width="400"></p>

### 계정과 사용량

Claude와 ChatGPT·Codex 계정을 여러 개 연결.
5시간·주간·모델별 한도와 리셋 시각, 크레딧, 리필권을 한 화면에 표시.

<p align="center"><img src="docs/images/settings-usage.png" alt="사용량 화면" width="840"></p>

### 알림

그래프마다 사용량 기준(예: 80%)이나 리셋 전 시점(예: 30분 전)을 추가.
한도 도달·리셋, 로그인 만료, 리필권 만료 알림도 선택 가능.

<p align="center"><img src="docs/images/settings-alert.png" alt="알림 추가" width="840"></p>

### 화면 설정

테마(시스템·라이트·다크), 언어(한국어·English), 로고 색, 위젯 막대 길이 변경.

<p align="center"><img src="docs/images/settings-light.png" alt="라이트 테마와 영어 화면" width="840"></p>

## 설치

필요한 것: 인터넷 연결, Claude 또는 ChatGPT 계정.

### Windows 10 / 11

[최신 릴리스](https://github.com/Ba-koD/usAgI/releases/latest)에서 `usagi.exe`를 받아 실행. 설정은 `%APPDATA%\usAgI`에 저장.

포터블로 쓰려면 `usagi.exe` 옆에 `data` 폴더를 만든 뒤, 그 안에 빈 파일 `usagi.portable`을 만들고 실행. 설정과 사용량은 `data`에 저장.

> [!NOTE]
> 포터블에서 계정을 추가하면 `data\profiles`에 공식 CLI의 로그인 파일이 생김. `data` 폴더는 다른 사람과 공유 금지.

### macOS

[최신 릴리스](https://github.com/Ba-koD/usAgI/releases/latest)의 `usAgI-mac.zip`을 풀거나 아래 명령으로 설치.

```bash
curl -fsSL https://raw.githubusercontent.com/Ba-koD/usAgI/main/scripts/install-mac.sh | bash
```

업데이트도 같은 명령으로 실행.

### 처음 실행

1. 위젯을 더블클릭해 설정 창 열기 (macOS는 메뉴 막대 아이콘 → 설정)
2. 사용량 화면의 **추가**에서 Claude 또는 ChatGPT·Codex 선택
3. 브라우저에서 로그인 승인. 이 PC에 이미 로그인된 CLI가 있으면 바로 연결

## 사용법

| 동작 | 결과 |
|---|---|
| 위젯 더블클릭 | 설정 창 |
| 위젯 우클릭 | 퍼센트 ↔ 리셋까지 남은 시간 |
| 알림 영역 아이콘 우클릭 | 설정 열기 · 위젯 표시 · 새로고침 · 종료 |
| 그래프의 **알림 추가** | 사용량 기준이나 리셋 전 시점 알림 추가 |
| 계정 카드 왼쪽 ⠿ 끌기 | 계정 순서 변경 (위젯도 같은 순서) |

## 작동 방식

```mermaid
flowchart LR
    CLI["공식 CLI<br>claude · codex"] -- 사용량 --> APP["usAgI"]
    APP --> WIDGET["작업 표시줄 위젯<br>메뉴 막대"]
    APP --> ALERT["알림"]
```

1. 유효한 기존 파일 로그인은 바로 연결. 새 로그인은 공식 CLI에서 브라우저로 진행. CLI가 필요한 경우 설정에서 공식 설치 안내 열기
2. 앱 시작과 설정창을 열 때 유효한 파일로 로그인 상태 확인. 설정한 주기(기본 5분)에 HTTPS로 사용량 조회. Claude는 사용할 수 있는 파일 세션이 없을 때 `/usage` 사용
3. Claude 계정의 **실시간 연결 켜기**를 선택하면 Claude Code 사용 중에 메시지마다 갱신
4. 받은 수치를 위젯에 표시하고, 계정의 알림 설정에 따라 사용량·리셋 알림 전송

**계정 제거**는 usAgI에 저장된 계정과 사용량을 제거. usAgI 전용 프로필의 로그인 파일, 대화 기록, 설정을 지우려면 **프로필·기록 삭제** 선택. 계정 제거 후 남은 프로필은 설정의 **일반 → 보관된 프로필**에서 정리 가능.

## 리소스 사용량

0.2.0을 실제로 실행한 상태에서 60초 동안 측정한 값.

| 프로세스 | 메모리 | CPU |
|---|---|---|
| Windows 본체 (위젯 + 알림 영역) | 약 8MB | 0.1% 미만 |
| Windows 설정 창 (열려 있을 때만) | 약 7MB | 0.1% 미만 |
| macOS 본체 (메뉴 막대) | 약 14MB | 0.1% 미만 |

- 메모리는 작업 관리자와 활성 상태 보기에 표시되는 값 기준. Windows는 계정 2개를 연결한 상태
- 설정 창은 별도 프로세스로 실행되고, 닫으면 종료
- 위 수치는 usAgI 프로세스 기준. 파일 세션을 쓸 수 없어 실행한 CLI의 리소스는 별도

## 개인정보와 보안

- **로그인 정보**: 공식 CLI가 보관. usAgI는 사용량을 조회할 때 파일의 액세스 토큰을 메모리에서만 사용
- **네트워크**: Claude의 `api.anthropic.com`과 Codex의 `chatgpt.com`에서 사용량 조회. GitHub에서 업데이트 확인·내려받기
- **업데이트**: 이 저장소의 릴리스 파일만 허용. 체크섬과 서명을 모두 확인한 뒤 설치
- **프로필 폴더**: 계정마다 만드는 폴더는 이 사용자만 접근 가능하게 생성

자세한 내용은 [개발 문서](docs/DEVELOPMENT.md) 참고.

## 문서

- [개발 문서](docs/DEVELOPMENT.md): 사용량 출처, 업데이트와 서명, 빌드, 릴리스, 코드 구조
- [변경 내역](CHANGELOG.md)
