# 뿌요뿌요 DA! (드림캐스트) 한글 패처

드림캐스트용 《뿌요뿌요 DA! -featuring ELLENA System-》(1999) 일본판 GDI에 한글 그래픽을 넣는 Rust 코드입니다. 이 게임의 일본어 문장은 모두 PVR 텍스처에 그려져 있어 폰트 훅 없이 텍스처를 교체합니다. GDI·ISO 9660 읽기와 같은 크기 파일 교체, EDC/ECC 재계산, PVR 디코드·인코드, 설정 파일 기반 한글 조판, 클린 플레이트와 레터링 이미지 합성, Universal Dreamcast Patcher용 DCP 생성을 제공합니다.

배포용 패치와 적용 방법은 [뿌요뿌요 시리즈 한글 번역 프로젝트](https://github.com/mcpads/puyo-puyo-kr-patch/tree/main/dc-puyopuyo-da)에서 제공합니다.

## 제공하지 않는 것

이 저장소에는 원본 GDI, 패치를 적용한 GDI, 텍스처별 번역 문구, 조판 설정 파일, 폰트 파일과 생성 그래픽(클린 플레이트·로고 오버레이·레터링 이미지)이 없습니다. 따라서 이 저장소만으로는 패치를 만들 수 없습니다. 아래 입력을 직접 갖춘 경우에만 한글 텍스처와 DCP가 생성됩니다.

아래 입력을 갖춰 [패치 생성](#패치-생성) 명령으로 만든 DCP는 텍스처 50개를 교체하며, 배포 패치 v1.0.0(SHA-256 `f117463518a6b8ed6907d99cabb487910a234d70bc4342cf999fd908eedd56d5`, 1,341,928바이트)과 바이트 단위로 같습니다.

## 빌드와 테스트

```bash
cargo build --release
cargo test
```

기본 테스트는 합성 입력만 사용합니다. 폰트가 필요한 테스트는 `#[ignore = "requires ..."]`로 필요한 파일을 밝혀 두었습니다. 입력을 갖춘 뒤 `cargo test -- --ignored`로 실행하며, 입력이 없으면 성공으로 넘어가지 않고 실패합니다.

## 지원 원본

일본판 3트랙 GDI입니다. 제품 번호 `T6601M`, 버전 `V1.003`입니다.

| 파일 | 크기 | SHA-256 |
| --- | --- | --- |
| `disc.gdi` | 87바이트 | `70b7e0c8a0d0b8554a3056c16726b85412d20f3398f2dce6cda5483265d89a3e` |
| `track01.bin` | 1,914,528바이트 | `c61e3ee701f17e55a62444c5f22b1d2e47dc5f287d38280015b97703be3ccc77` |
| `track02.raw` | 1,735,776바이트 | `8364bc87848527f027fa6a9d13cae778c0132f3704dff6978173d92df1d71e06` |
| `track03.bin` | 1,185,760,800바이트 | `ac07913d7179a2d8f1df9f198dc3e6cceb0d78518402f169472453870cc6440b` |

빌드는 디스크 전체 해시를 검사하지 않습니다. 위 표와 다른 디스크에서 만든 결과는 지원하지 않습니다.

## 빌드 입력

조판 설정 파일 안의 경로는 설정 파일이 있는 디렉터리의 상위 디렉터리를 기준으로 읽습니다. 기본 배치는 다음과 같습니다.

| 입력 | 기본 경로 | 비고 |
| --- | --- | --- |
| 조판 설정 | `assets/font_config.json` | 대상 텍스처, 글꼴·크기·좌표, 클린 영역 (`--config`로 변경) |
| 번역 문구 | `assets/translations/raw/*_kr.txt` | 텍스처별 한 줄 단위 문구 |
| 생성 그래픽 | `assets/generated/*.png` | 일본어를 지운 클린 플레이트, 로고 오버레이, 난이도 레터링 이미지 |
| 제목·로고 폰트 | `assets/fonts/BMJUA.ttf`, `assets/fonts/Jalnan2.ttf` | 배민 주아체, 여기어때 잘난체 2 |
| 본문 폰트 | `assets/fonts/NEXONLv2Gothic.ttf`, `NEXONLv2GothicMedium.ttf`, `NEXONLv2GothicBold.ttf` | 넥슨 Lv.2 고딕 |
| 합격·실격 문구 폰트 | `assets/fonts/Galmuri11.ttf` | [Galmuri](https://github.com/quiple/galmuri) |
| 픽셀 이름표 폰트 | `assets/fonts/neodgm.ttf` | [Neo둥근모](https://github.com/neodgm/neodgm) |

폰트는 재배포 조건을 이 저장소에서 보장할 수 없어 포함하지 않습니다. 각 폰트의 라이선스는 배포처에서 확인하세요. 배포 패치 v1.0.0은 다음 폰트 파일로 만들었습니다.

```text
e8e6aa8b1b662c7bf0d7f136f29e822e0985176458a6e5d0ba08afc4a5c901a9  BMJUA.ttf
bd0028223d1e69c0bf76266bee52931be26a5f8a9fce517bb8765371d57836a3  Jalnan2.ttf
389ad546769c0cb958b1c5c5c1d4b473867b433e0a6697b01907c7d7e1565c60  NEXONLv2Gothic.ttf
7a578b435ef384e56bb93c8bd8f2412ebdaeefd2befba5bd81f35f4bce2030b8  NEXONLv2GothicMedium.ttf
7d538770c259b287b1e94d3e4fcb55bac71d866dbc98a69e0aab1fbfb7c6dade  NEXONLv2GothicBold.ttf
2c709890595668f7bdb6df408420fda957dde0288e95b31a1cc17a2ab98b4b4f  Galmuri11.ttf
d61b60eccb731f8ca9c7da582e4a05a94db66b570471809950aa9a7261b941d6  neodgm.ttf
```

## 패치 생성

```bash
B=target/release/dc-puyopuyo-da-kr-patch

# 원본 미디어 검사
$B verify-media roms/JP/disc.gdi

# 설정된 텍스처를 조판해 교체 PVR과 replacements.txt 생성
$B typeset-configured roms/JP/disc.gdi --config assets/font_config.json --output-dir out/typeset

# 교체 PVR을 넣은 GDI 재구성 (EDC/ECC 재계산 포함)
$B build-batch-gdi roms/JP/disc.gdi --replacements out/typeset/replacements.txt --output-dir out/rebuilt

# 원본과 비교해 바뀐 루트 파일만 담은 DCP 생성
$B build-dcp roms/JP/disc.gdi out/rebuilt/disc.gdi --output out/patch/puyopuyo_da_kr.dcp
```

DCP는 바뀐 ISO 루트 파일 전체를 담은 ZIP입니다. 원본에서 파일이 삭제된 경우는 이 형식으로 표현할 수 없어 실패합니다. 생성한 DCP는 [Universal Dreamcast Patcher](https://github.com/DerekPascarella/UniversalDreamcastPatcher)로 원본 GDI에 적용합니다.

## 그 밖의 명령

`info`, `list-files`, `diff-files`, `extract-file`, `pvr-scan`, `pvr-dump`, `pvr-palette`, `compare-configured`, `prepare-overlay` 등의 사용법은 `cargo run -- help <명령>`으로 확인할 수 있습니다.

## 라이선스

이 저장소의 소스 코드는 [MIT License](LICENSE)로 제공합니다.
