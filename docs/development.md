# 開発者向け資料

このプロジェクトに手を入れる人向け。
使い方は [usage.md](usage.md)、デバイスの仕様は [device.md](device.md)。

## 概要

- 実装は Rust (`src/`、Cargo workspace、edition 2024、resolver 3)
- 視聴に使うのは System1 の経路 (trc ファームウェア + EP 0x81)。
  System2 (EP 0x86) は実装していない。
- 大きい未解決の課題は CS ND08 の視聴中のドロップ (下の "未解決の課題" セクション)

## 構成

| crate | 種類 | 内容 |
|---|---|---|
| `src/dt295-usb` | lib | USB アクセス。Windows は windows-sys で WinUSB を直接、Linux は nusb |
| `src/dt295` | lib | ドライバーのコア (ファームウェア、認証、フロントエンド、System1 ストリーム)、I/O の無い TS 処理、状態ファイル |
| `src/dt295-bondriver` | cdylib | `BonDriver_DT295.dll` (中身は Rust、`IBonDriver2` は C++ の薄い shim)。`bontest` と見本の ini もここ |
| `src/dt295-cli` | bin | `dt295` のサブコマンド (Linux では `serve`、`tune` も) |

| ファイル | 役割 |
|---|---|
| `dt295-usb/src/` | 制御・バルク・割り込み転送、EP 0x81 の読み出しキュー `ReadQueue`、個体の一覧 |
| `dt295/src/device.rs` | 公開 API `Device` と、ほかのスレッド向けの `Live` (ストリーム中の C/N など) |
| `dt295/src/{proto,fw}.rs` | レジスター、doorbell、コマンドと ACK 待ち。ファームウェアの起動と、残った状態からの回復 |
| `dt295/src/{auth,cas,dtvdec}.rs` | 起動時の認証、セキュアセッション (keepalive、B-CAS の第 2 段)、リンク鍵 |
| `dt295/src/{i2c,fe1,signal}.rs` | I2C、フロントエンド (選局、ロック待ち、C/N) |
| `dt295/src/s1/` | System1 ストリームと worker (下の "仕組み" セクション)。`decoder_init.bin` は純正の初期化のキャプチャで、手で編集しない |
| `dt295/src/ts/` | I/O の無い TS 処理 (PAT、PMT、セクションの組み立て、リンク暗号、PSI keeper、全サービスのルート `route.rs` など)。単体テストの大半はここ |
| `dt295/src/watch.rs` | ストリームを始め直すかの判断 (`CardWatch`: B-CAS カードと止まったファームウェア。後者のみなら `StallWatch`) |
| `dt295/src/{state,chan}.rs` | 状態ファイル。地デジのチャンネル表と衛星の ini の行 (BonDriver と `dt295` が共有) |
| `dt295-bondriver/src/{ffi,driver}.rs`, `cpp/` | `CreateBonDriver` と shim。`IBonDriver2` の中身 (選局、ストリーム、idle keeper、回復) |
| `dt295-bondriver/src/{config,channels,persist,fifo,logging}.rs` | ini、チューニング空間、状態ファイルの規則、TS の FIFO、ログ |
| `dt295-cli/src/` | サブコマンドごとのファイル。`tuner.rs` は `capture`、`logo`、`serve` が共有する個体の起動と停止の合図、`bonini.rs`、`ch2.rs`、`sections.rs` は ini、.ch2、TS のパケットの読み取り |
| `dt295-cli/src/serve/`, `tune.rs` | `dt295 serve` と `dt295 tune`。`conf` と `proto` はどの OS でもビルドしてテスト |
| `packaging/arch/` | Arch Linux のパッケージ (PKGBUILD、systemd、sysusers、tmpfiles、udev の設定) |
| `packaging/examples/` | mirakc 向けの局ロゴの受信のスクリプトと systemd の timer |
| `tools/` | 純正ドライバーの USB 通信の観測 (USB ETW) |

設計上の決まり:

- System2 の経路 (EP 0x86、trc ファームウェアなし) は無い。
  `TrcFirmware` が無いとストリームは始まらない。
- ストリーム中にデバイスと I/O するのは worker のみ。
  ほかのスレッドは待つか、読み取り専用の値を参照するのみ。
  `GetSignalLevel` は worker が毎秒読んだ値を返し、デバイスには触らない (TVTest は常に呼ぶので、ここで I/O すると止まった個体にアクセスが集中する)。
- nusb は Windows では使わない。
  WinUSB の RAW_IO を常に有効にし、`PIPE_TRANSFER_TIMEOUT` も設定できないため、実機で確認した読み方 (同期 ReadPipe + タイムアウト、RAW_IO 付きの 16 本のキュー) を再現できない。
- C++ の shim が必要。
  TVTest は `CreateBonDriver` の戻り値を `dynamic_cast<IBonDriver2*>` するので、MSVC の RTTI 付きのクラス (x86 では thiscall) が必要。
  `CreateBonDriver` 自体は Rust がエクスポートする。
  FFI の入り口はすべて `catch_unwind` で panic を止める。
- CRT は静的リンク (`.cargo/config.toml` の `+crt-static`)
- `dt295 driver install` は、デバイスのセットアップクラスを USBDevice にしてから、Windows 付属の `winusb.inf` の「WinUsb Device」を `DiInstallDevice` でインストールする。
  純正ドライバーは WHQL 署名で優先度が高く、新しく挿した個体には毎回それが選ばれるので、個体ごとに一度 `install` が必要。

## ビルドとテスト

```
cargo build --release                                   # x64
cargo build --release --target i686-pc-windows-msvc     # x86
cargo test --workspace                                  # デバイス不要
cargo clippy --workspace --all-targets
cargo check --target x86_64-unknown-linux-gnu -p dt295-usb -p dt295 -p dt295-cli   # Linux 向けのビルド確認
cargo test -p dt295-usb -- --ignored                    # 実機のスモークテスト (ほかのプロセスが個体を使っていないこと)
```

- 実機での確認は `bontest` ([usage.md](usage.md))。
  `scan 3:0 2:0 0` のように衛星の後に地デジを選局すると、選局の切り替えまで確認できる。
- 試験用に DLL を別のフォルダーに置くときは、ini の `Firmware`、`TrcFirmware`、`StateFile` を TVTest のフォルダーの絶対パスにすると、TVTest の状態ファイルを共有可能
- バージョンは `src/version.rs` がビルドのときに作る (例: `0.1.0.r126.ge4c1e81`)。
  形は PKGBUILD の `pkgver` と同じで、`Cargo.toml` の version、コミット数、コミットを並べる。
  ソースに未コミットの変更があると `+dirty` が付き、git が使えないときは `Cargo.toml` の version のみになる。
  Windows の DLL と exe には `rc.exe` でバージョン情報のリソースを埋め込む (ファイル バージョンの 4 つ目はコミット数)。
- `packaging/arch/` の PKGBUILD は HEAD を clone してビルドし、`check()` で `dt295-usb`、`dt295`、`dt295-cli` のテストを Linux で実行
- `dt295 etw-urbs` は、`tools/usb_etw_capture.cmd` で取った純正ドライバーの USB ETW をアクセスの一覧に変換 ([device.md](device.md) の "純正ドライバーの通信の観測" セクション)

## 仕組み

### 起動 (`OpenTuner` -> `Device::start`)

起動前のファームウェアの状態によって処理を分ける:

- 0 (未ロード): コールドブート (idle ファームウェアを転送)
- 0x10: そのまま次へ
- 0xFFFF: EP 0x81 に残った TS を読み捨て (`fw::drain_ts`)、cmd3 で 0x10 へ
- ストリーム中 (0x01 / 0x02 / 0x04): 通常の停止手順 (`s1::to_idle`) で 0x11 に戻し、以降は次の 0x11 と同じ
- 0x11: idle ファームウェアをその場で再ロードして 0x10 にする (`fw::reload_idle`)。
  失敗したら cmd(2,1) -> cmd(2,0) で 0x10 に戻す。
- 0x12: cmd(2,0) で 0x10 へ

その後、認証 -> IDLE パラメーター -> cmd1 (-> 0x11) -> チューナーのリセットと B-CAS の初期化 -> フロントエンドの初期化。
状態ファイルのリンク鍵は `Device::start` の前に渡す (`Device::set_prior_link_keys`)。

### 選局 (`SetChannel`)

1. 前のストリームを止める。
   ファームウェアが止まっていれば再初期化し、失敗したら、再オープンされるまでは再試行しない。
2. 選局してロックを待つ。
   待つのは地デジで最大約 3.4 秒 (復調 reg 0x80 の bit 7 が 16 回続けて立っていたら、約 1 秒で空きチャンネルとして打ち切る)、衛星で最大約 8 秒。
3. ロックしたチャンネルのみストリームを始める。
   `SetChannel` が TRUE を返すのは、ストリームが動いたときのみ。
   ストリームの開始は全体で 7.5 秒以内 (ホストのタイムアウトは 10 秒で、2.5 秒は下の PSI のみのフォールバック用)。
   - ini の `ServiceID` の無いチャンネル: 全サービス (下の "全サービス" セクション)
   - `ServiceID` のある行: single-service で直接選局する。
     未契約の有料サービスなどで 1.2 秒何も届かなければ、その TS を full-mux の PSI のみで流す (`s1::psi_fallback`)。
     ホストからはサービスが見え、スキャンに載る。
4. 選局の途中でファームウェアが止まったら、その場で再初期化して、1 度のみ選局をやり直し (再初期化は約 1 秒)

### 全サービス

レジスターと、デバイスの振る舞いは [device.md](device.md) の "ルートのスロット" セクション。

- サービスを指定しないとき、デコーダー窓を AUTO_PID off、PROGRAM_NUMBER 0、0x101A の bit 12 を 1 にして、ルートのスロットを明示的に書く (`s1::Sel::Routes`)。
  初期化が入れたスロットと ECM の割り当てを消してから、最初のルート (NULL と SI) を書き、`cmd(4,0x20,0)` -> `cmd(5,0,2)`。
- worker が届いた TS の PAT と PMT を解析して (`ts::route::Router`)、PMT、ECM、PCR、ES のルートを、ストリームを止めずに追加する (`s1::route_write`)。
  読み出しを止めないよう、1 回に 4 スロットまで。
  PMT が変われば、不足している PID を追加する。
- 優先順は NULL と SI、PMT と ECM、PCR、映像と音声、字幕と文字スーパー (stream_type 0x06)、データ放送 (0x0D)。
  59 スロットに入らない分はログに `PID(s) got no slot` と出して捨て、不要になった PID のスロットは、空きが無くなったときに再利用する。
- スクランブルされている PID は、PMT の CA_descriptor (B-CAS、CA_system_id 5) の ECM のスロットに結び付ける。
  ES の記述子が番組の記述子より優先で、ECM の PID が 0x1FFF ならスクランブル無し。
- ECM のルートは 4 つまで一度に書き、5 つ目からは前の ECM の 0.5 秒後に書く (`worker::ECMS_AT_ONCE`、`ECM_GAP`)。
  待っている ECM のスロットへの書き込みと、そのスロットに結び付く PID は ECM と一緒に待ち、ほかのルートは先に書く。
- ストリーム中に `Device::needs_recovery` が立ったら、`dt295 serve`、`dt295 capture`、BonDriver の idle keeper が `Device::start` から起動し直して、同じチャンネルのストリームを始め直す (`StallWatch`。中断は約 3 秒)。
  1 分以内に続けて 3 回までで、その後は前回から 1 分待つ。
- 映像と音声の判定 (`EsCheck::observe_av`) は、ECM の付いた映像と音声の PID 全体で判定する。
  ワンセグはスクランブルされていないので、カードの判定に使えない。

### 衛星: サービスの絞り込みと PSI keeper

- サービスの絞り込み: single-service のストリームでも、デバイスは PAT と SDT[actual] に TS の全サービスを載せてくる。
  そのままだと TVTest が TS の全サービスをその行に割り当て、ほかのサービスは何も映らない。
  衛星ではデバイスの PID 0x0000 / 0x0011 のパケットを NULL にし、PAT を NIT + 選局したサービス、SDT[actual] を選局したサービスのみに絞って送り直す。
  全サービスのときは PAT と SDT をそのまま出力する。
- PSI keeper: PAT、PMT、NIT[actual]、SDT[actual|other] を、状態ファイルを通してプロセスをまたいでキャッシュする。
  未契約の有料サービスなどで TS にそれらが含まれない間は、キャッシュを再送する (PAT を見たことが無ければ、SDT のサービスの一覧から作る)。
  PMT は作らない: TVTest のスキャンは、PMT はあるが再生できる映像が無いサービスを落とし、PMT が届かないサービスは登録する。
  そのストリームでデバイスがまだ出していない表のみを再送する (デバイスも使う PID に重ねると連続性カウンターが崩れ、TVTest がドロップと数える)。

### リンク鍵

デバイスの鍵の扱いは [device.md](device.md) の "TS ストリーム (System1 / EP 0x81)" セクション。

コアはセクションの先頭の PAT ごとに、使っている鍵で復号して CRC を検証し、通らなければほかに知っている鍵を試し、通った鍵に切り替える (`s1::link_key_check`)。
切り替えはその PAT から効く (`s1::link_decrypt`)。
ホストは検証済みの鍵 (`Device::link_key`) を状態ファイルに保存する。

セッションが切れた後の起動で渡された鍵は、検証前でも適用されている。
その起動が検証前に終了しても鍵が失われないように、コアは鍵を渡した時点でホストに知らせ (`Device::set_link_key_journal`)、ホストはそれを `linkkey-pending` として状態ファイルに書く。
次のプロセスは検証済みの鍵と未検証の鍵を順に試し (`Device::set_prior_link_keys`)、通った鍵を `linkkey` に書く。
未検証の鍵は、そのプロセスが渡した鍵がまだ検証されていなければそれのみを残し (`Device::unproved_link_key`)、ほかは消す。

### 状態ファイル

形式は [state.rs](../src/dt295/src/state.rs) の先頭のコメント。

- 変更があったときのみ書き込み (ストリームの停止時、`CloseTuner`、idle keeper が約 3 秒ごと)
- 90 日間受信していない PSI セクションは、読み込み時に破棄

### C/N (`GetSignalLevel`)

ストリーム中は worker が毎秒、復調チップを読む。
`GetSignalLevel` はその値 (`Live::cn_mdb`) を返す。
3 秒以内のサンプルが無ければ直前の値のまま、ストリームが無いときは選局時に読んだ値。

### USB の読み出しと keepalive

- worker は EP 0x81 に 64 KiB x 16 本の読み出しを常時投入 (`ReadQueue`、WinUSB の RAW_IO)
- ファームウェアのイベントのポーリングは最短 100 ms 間隔 (毎回の読み出しの後だと、1 秒に約 240 回の制御転送になる)
- パケットの先頭ごとに同期バイトを確認し、ずれたらその手前までを渡して区切りを探し直す。
  single-service のストリームは、デバイスの PAT がそのサービスを載せるまで何も渡さない。
- keepalive は 12 秒ごと ([device.md](device.md) の "認証とセキュアセッション" セクション)。
  セッションの最初の 1 回はストリームが動き出した時点で送り、ストリームが無ければ認証の 5 秒後 (3 秒後だと short challenge が返ることがある)。
  ストリーム中は worker が、アイドル中はホストが 1 秒ごとに `Device::idle_tick` を呼ぶ。
  失敗 (`short challenge`) したら 4 秒後に 1 回のみ送り直す。

### B-CAS カードと USB の抜き挿し

状態の値は [device.md](device.md) の "認証とセキュアセッション" セクション、カードのセッションの振る舞いは "TS ストリーム (System1 / EP 0x81)" セクション。

- worker は割り込みの bit 9 で B-CAS の状態を読み、`Live::card_events` を増やす。
  core は第 2 段の後と、ストリームの開始から 3 秒後にも読み (`Live::card_state`)、3 秒後に 5 なら警告する。
- 映像と音声の有無 (`ts::es::EsCheck`): 自分のサービスの PMT にある映像と音声の PID に、2 秒の間に PES の先頭 (00 00 01) が 1 つも無ければ「無い」で、2 回続くと `Live::es_clear` が `Some(false)` になる。
  映像も音声も無いサービスと、PSI のみのフォールバックは判定しない。
- 復帰の手順 (`CardWatch`。BonDriver の idle keeper と `dt295 serve` が使う):
  - 最後のイベントから 5 秒待ち、カードがあれば、ストリームを止めてカードのセッションをその場で張り直す (`Device::resume_in_place`)。
    待機中に第 2 段のみを送り、3 秒以内に状態が 2 か 3 になれば、選局はそのままでストリームのみを始め直す (中断は約 0.5 秒)。
  - 張り直せなかったら、`Device::start` からやり直して今のチャンネルを再選局 (中断は約 3 秒)
  - それでも映像と音声が無ければ、10 秒、20 秒、40 秒後に `Device::start` からもう一度 (1 回の抜き挿しで最大 4 回)
  - イベントが無くても、映像と音声が無いまま 15 秒たてば、選局ごとに 1 回は `Device::start` からやり直し
  - 状態が 0 の間はやり直さない。
    状態が 3 で映像と音声が無いのは契約が無いサービスなので、やり直さない (ログは `no contract for it?`)。
    自分のやり直しの 5 秒以内のイベントは数えない。
- USB を抜くと、worker の読み出しが「no device」で失敗し、`Live::device_lost` が立つ。
  BonDriver の idle keeper はデバイスを閉じ、2 秒ごとに `dt295::present_serials` で探して、見つかったら再オープンして今のチャンネルを再選局する。
  ストリームが無いときは 5 秒ごとに抜かれていないか確認する。

### Linux のデーモン (`dt295 serve`)

`dt295 capture` を録画ソフトのチューナーのコマンドにすると、起動のたびにファームウェアを起動して認証し、録画ソフトに SIGKILL で止められる。
`dt295 serve` は個体ごとのスレッドがデバイスを開いたまま保持し、クライアントの `dt295 tune` は選局を要求して TS を読むだけにする。
設定は [usage.md](usage.md) の "`dt295 serve`" セクション、ソケットのプロトコルは [serve/proto.rs](../src/dt295-cli/src/serve/proto.rs) の先頭のコメント。

- クライアントはソケットの場所を `/etc/dt295.conf` から読み込み (`serve::conf::client_socket`)
- アイドル中に `if-free` の要求とほかの要求が同時に来たら (`yield_to_queued`)、`if-free` の方に `busy` を返す。
  `if-free` のストリームの開始中に来た要求は、開始が終わってからストリームのループで受けて切り替える。
- 個体のスレッド (`serve/unit.rs`): 開けるまで 5 秒ごとに再試行する。
  アイドル中は毎秒 `idle_tick`、5 秒ごとに `dt295::present_serials` (開かずに列挙する) で抜かれていないか確認し、変わった鍵と PSI を保存する。
  要求が来たら、ファームウェアが止まっていれば、または前のストリームでデバイスが 10 秒以上何も出力しなかったら、再初期化してから選局する。
  途中でファームウェアが止まったら、1 度のみ再初期化してやり直す。
- TS は worker の sink から上限付きのキュー (64 KiB の読み出し 256 個分) を通り、書き込みスレッドがソケットへ書く。
  読み手が遅れても worker は止まらず、あふれた分は捨てて数える。
  書き込みが 30 秒進まなければ、相手がいなくなったとみなす。
- ストリームを止めるのは、接続が切れたとき、新しい要求が来たとき、個体が抜かれたとき、終了の合図のとき。
  相手の切断は、書き込みの失敗のほか、200 ms ごとにソケットを `MSG_PEEK` で調べて検出する (サービスの PAT が届くまで何も書かないため)。
- `dt295 tune` はソケットと標準出力を poll で同時に監視し、読み手が出力を閉じたら TS が届いていなくても終了する。
  標準出力へは行バッファーを通さずに書く (Rust の stdout は 0x0A のたびに書き出すため)。
- カードの抜き挿しやファームウェアの停止で始め直すときは、同じクライアントへの送信を継続
- 再初期化の判定に使う「デバイスが出した量」は `Live::delivered` で、PAT を待つ間に捨てた分も入る

### チャンネルスキャン (`mirakc-channels --scan`)

すべて `dt295 serve` への `if-free` の要求で、録画中なら `busy` で止まる (`mirakc/{scan,psi}.rs`)。

- 地デジは UHF (と `--catv` の CATV) を 1 つずつ選局し、PAT に載る TV とラジオのサービスと、SDT の network_id を取得する。
  ロックしないチャンネルは約 4.5 秒で `error no lock`。
- 衛星はネットワークごとに既知の TS (BS15 0x40F1、ND02 0x6020、ND04 0x7040) を 16 秒読み、その SDT[other] にある別の TS をもう 1 つ読んで、最初の TS のサービスも集める。
  SDT[other] は約 13 秒で 1 周し、CS110 はネットワーク 6 と 7 で別。
- TV とラジオ (service_type 0x01 / 0x02) をすべて出力する。
  SDT のフラグは本サービスと予備のサービス (BS朝日 151 と 152 など) で同じで、見分けに使えない。
- `--on-air` のときは 1 つずつ選局し、4 秒以内に PAT が届いたもののみ保持

### 局ロゴ (`dt295 logo`)

ロゴの送出の状況と使い方は [usage.md](usage.md) の "`dt295 logo`" セクション。
TVTest のロゴファイルの形式は [logo/tvtest.rs](../src/dt295-cli/src/logo/tvtest.rs) の先頭のコメント。

- BS / CS のロゴは、サービス 929 (BS-15、TSID 0x40F1、service_type 0xA4) のデータカルーセル。
  component_tag 0x79 が BS (モジュール LOGO-00..05)、0x7A が CS (CS_LOGO-00..05) で、1 モジュールが 1 つの大きさ (logo_type 0..5) の全局分。
  1 回の送信は BS 約 9 秒、続けて CS 約 30 秒で、それ以外の時間はこの PID に 1 パケットも届かない。
  ロゴの PID は 0x0779 (BS) と 0x077A (CS)。
  929 は single-service で選局すればカルーセルが通る。
- 地デジのロゴは各局の CDT (PID 0x29、table_id 0xC8、data_type 0x01) で、single-service の出力も通す。
  セクションは 1 つが logo_type 1 種類分 (226-322 B で 2 パケット) で、約 100 秒ごとに始まり、2 つ目のパケットは約 50 秒後に届くので、6 種類の 1 周は約 10 分。
  サービスとロゴの対応は、SDT の logo_transmission_descriptor (0xCF、伝送種別 1 と 2 の logo_id) で取得する。
- `dt295 logo` は PAT -> PMT (stream_type 0x0D) -> DII / DDB でモジュールを組み立てる。
  正しい PAT が 15 秒届かなければ個体を解放してやり直し、DII の download_id が変わったら古いモジュールは捨てる。
  受信を待つ間に TVTest がロゴファイルを書き換えることがあるので、マージの直前に読み直す。
  `--schedule` は schtasks に XML で登録する (StartWhenAvailable、InteractiveToken、最長 3 時間)。
  `--log` は標準出力と標準エラー出力をファイルに向け直してからコンソールを離す。
- mirakc 向けの種類は mirakc が放送から取得するときの優先順 (5、2、4、1、3、0) で、93 B 以下の透明のダミーを除いて選ぶ。
  ファイル名の `<network_id * 100000 + service_id>` は mirakc のサービス ID。
- 929 の無い TS は CDT を読む (`logo/cdt.rs`)。
  終わるのは、SDT が指定するロゴがどれも 6 種類そろったときか、最初に届いたロゴがもう一度届いたとき。
  どれかのロゴが 2 度届いたのみでは終わらない。
  CDT のロゴは TVTest には足さない (TVTest は視聴中に自分で取得する)。
- `--ts -` の読み取りは別スレッドで、入力が止まっても `--wait` で終了

### ログ

BonDriver は `[Tuner] LogFile=1` でファイルにも書く。
主な行:

- 選局ごと: `SetChannel <MHz>: LOCK|nolock cn=...dB (raw ...) | timing ms: stop=... tune=... start=... total=...`
- ストリームの停止時: `s1 worker: delivered N bytes, P PAT(s), ...` と `s1 worker: N read(s) (M full 64K), longest gap ... ms; K continuity error(s) in the device's TS...; T TEI packet(s)`
- 異常 (連続性エラー、TEI、100 ms を超える読み出しの間隔、復調の誤り、アンロック) のあった分のみ: `s1 stats min N: ... continuity error(s) on PID ..., TEI ..., error seconds: ..., longest gap ... ms, ... MB; demod C/N ..., err 0xEB sum ... max ..., 0xF0 sum ... max ...`
- 衛星のストリームの停止時: `psi: TS xxxx -- re-sent PAT xN, PMT xN, SDT xN, NIT xN | cache: ...`

## 実機での確認

日付と個体を書いていない行は 2026-10-19、個体 CD23456789。

| 項目 | 結果 |
|---|---|
| 受信 (CLI、DLL x64 / x86) | 地デジ 27ch を 15 秒 54 MB、x86 の DLL で 8 秒 28.7 MB、BS と CS の映像と音声も出て、連続性エラー 0 |
| 長い受信 | TVTest で CS ND20 を 49 分 4.44 GB、LXC で全サービスの ND06 を 2.5 時間 43.5 GB (2026-10-31)。どちらも連続性エラー 0 |
| TVTest のチャンネルスキャン | 地デジ 38、BS 57、CS110 54 サービスを登録。90 ストリームすべて連続性エラー 0 |
| 地デジの空きチャンネルの打ち切り | UHF 50 チャンネルのスキャンが 177.4 秒 -> 66.9 秒 |
| 衛星の後の地デジ (`scan 3:0 2:0 0`) | CS、BS を選局した後の UHF のスキャンで 10 チャンネルすべてロック |
| 個体の選択、ドライバーの切り替え | `DeviceSerial` で指定した個体のオープン、`dt295 driver restore` と `install` の後の `dt295 capture` での受信を確認 |
| リンク鍵と前のストリームの残り (2026-10-24) | 抜き挿ししない再オープンは前のセッションの鍵で復号。強制終了後の起動は残りの TS 8 MB を読み捨てて、先頭から自分の PAT |
| 繰り返しの起動 (2026-10-26) | LXC で 300 回のストリームの開始と停止、DLL で 1 回開いたまま 230 回の選局で、すべて受信成功 |
| idle ファームウェアの再ロード (2026-10-26) | 再ロードの無い版は 0x11 から開いた 12 回のうち 3 回で停止、再ロードのある版は 13 回とも停止なし |
| 衛星の 1 サービスの行 | BS01/768 はデータカルーセル、BS13/531 は AAC 音声が流れ、PAT と SDT[actual] はそのサービスのみ |
| 全サービス (2026-10-29、30、11-03) | TOKYO MX の 6 番組、BS の 26 TS、CS110 の 12 TS (ECM 3-8 つ) で、映像のある番組をすべて同時にデスクランブル。スロットは BS で最大 42 個、CS110 で最大 51 個。TOKYO MX は 59 個を使い切り、データ放送の 3 PID が入らない |
| TVTest で全サービス (2026-10-30) | 地デジで音と映像が出て、TOKYO MX の MX1 と MX2、NHK 総合のサービスの切り替えも可能 |
| ストリーム中の PMT の変化 (LXC、2026-10-31、11-01) | 地デジ 27ch、BS05、ND10 で、増えた音声の PID も 5 秒以内に出力。連続性エラー 0 |
| ECM の数と停止 (ND06、2026-10-30、31) | 一度に追加する ECM が 4 つで 34 回のうち 0 回、5 つで 20 回のうち 1 回、6 つで 34 回のうち 2 回、7 つで 30 回のうち 12 回、8 つで約 90 回のうち約 40 回停止。8 つを 0.25-1 秒おきに追加すると 34 回のうち 2 回。流れ始めた後は、10 分の受信 12 回で停止 0 回 |
| ECM の間隔と始め直し (ND06、2026-10-31) | 4 つの後に 0.5 秒おきに追加して、60 回のうち 1 回停止し、3.2 秒後に復帰。60 回とも 8 番組をデスクランブル |
| 止まったときの始め直し (2026-10-31) | 一度に 8 つ足す版で `dt295 capture` と `dt295 serve` を 8 回ずつ流し、4 回で本物の停止が起き、3.2 秒で復帰。止まるときは続けて止まりやすく、4 回続いた回もあり。連続 3 回の後は前回から約 60 秒待機。BonDriver は ACK の停止を装った版で確認 |
| keepalive が切れるまでの時間 (2 台、2026-10-31) | 最後の keepalive から 60-61 秒で 0xFFFF (ストリーム中も、アイドル中も)。keepalive を止めたストリームも開始から約 64 秒で 0xFFFF。認証のみのセッションは 200 秒たっても維持 |
| CAS チャネル無し (2026-10-29) | 27ch、BS、WOWOW、CS、全サービスで映像と音声が出て、ストリームの開始の最後の段は 185 ms -> 93 ms |
| B-CAS カードの抜き挿し (2026-10-29、30、31) | 挿してから約 5.5 秒で映像が復帰 (中断は約 0.5 秒)。起動からやり直す経路では約 8 秒。契約の無いカードへの差し替えも同じ経路で復帰。BS-15 の TS1 の全サービスでは 6 番組が復帰し、連続性の飛びは PID ごとに 1 回のみ |
| 契約の無いカードで有料の TS (2026-10-30) | PAT と PMT は届き、映像と音声は無く、やり直しも無し |
| TVTest を開いたままの USB の挿し直し (2026-10-29) | 挿すと約 3 秒で再オープンし、同じチャンネルを再選局 |
| `dt295 logo` | 972 件が、TVTest が自分で受信したロゴファイルと全件一致 (--tvtest の待ちは 15 分) |
| BS / CS のロゴの送出状況 (2026-10-21、25、11-01) | 10-21 は 3-6 時に 20 分ほどおき、10-25 は 18 時台に受信。11-01 から 11-02 の 18 時間 (10:52-19:50、22:26-7:50) は 0x0779 / 0x077A に 1 パケットも届かず、同じ PMT の 0x0771-0x0778 は偶数時に到着 |
| Linux の VM (Debian 13、2026-10-23、24) | mirakc + EPGStation で 6 本 (最長 60 分 8.4 GB) を別のチャンネルへ続けて録画し、連続性エラー 0。USB パススルーでは起動に約 6 秒 |
| `dt295 serve` + mirakc (LXC、2026-10-25、30) | 要求から受信の開始まで約 2.1 秒。TS ごとの 48 チャンネルで、scan-services 6 分半、sync-clocks 5 分半、update-schedules 2 時間 (92 チャンネルでは 10 分、9 分半、2 時間 17 分) |
| `dt295 serve` の連続運転 (LXC、2026-10-31 から 11-03) | 65 時間で 950 回のストリームを流し、停止、始め直し、ERROR はすべて 0。メモリは 8.2 MB のまま増えず、update-schedules は 5 回続けて 48 TS を巡回 |
| `mirakc-channels --scan --on-air` (LXC、2026-10-25) | 103 チャンネル (9 分 29 秒)。TVTest のスキャンの 92 をすべて含み、残りは放送中だった BS の予備のサービス |

## 不具合と対処

一度起きた不具合のうち、コードを変更するときに再発させやすいもの。

| 症状 | 原因 | 対処 |
|---|---|---|
| 短いプロセスの選局の繰り返しで、ファームウェアが応答しなくなる | 最初の keepalive の前にプロセスが終了 | 最初の keepalive をストリームの開始時に送信 |
| 鍵を検証する前に SIGKILL されると、以後どの鍵でも復号できない | 検証済みの鍵しか状態ファイルに残していなかった | 未検証の鍵も保存 (上の "リンク鍵" セクション) |
| 録画に PAT と SDT が無い。残りが別のチャンネルのものだと mirakc が 404 を返す (`SID not found in PAT`) | 止めずに終わったストリームの TS が次の起動で先に出て、その PAT で鍵を決めていた | PAT ごとに鍵を確認。0xFFFF からの再初期化で残りを読み捨て (`fw::drain_ts`)。single-service はそのサービスの PAT まで渡さない |
| 残りとの境目でパケットの区切りがずれる | worker が 64 KiB の先頭しか同期バイトを見ていなかった | パケットごとに確認 (`s1::worker::deliverable`) |
| 放送休止中のチャンネルで `dt295 tune` が終わらず、mirakc のスキャンが 56 分かかる | mirakc はパイプを閉じるのみで、書き込むデータの無い `tune` もデーモンも切断に気づかない | `tune` は標準出力を poll し、デーモンは 200 ms ごとにソケットを確認 |
| 前のプロセスの終了から 1 分ほどで開くと、ファームウェアが停止 | 前のセッションが切れるのが、新しいセッションの認証の直後になる | 0x11 からは idle ファームウェアを再ロードしてから認証。それでも止まったら 1 度のみ再初期化してやり直し (`Unit::restart`)。mirakc は最初のデータを 16 秒しか待たないので、状態が 0xFFFF なら ACK のタイムアウトを待たない |
| ECM の多い TS で、開始直後にファームウェアが停止 | 不明 (一度に足す ECM が多いほど起きる) | ECM を間隔をあけて追加し、止まったら始め直し (上の "全サービス" セクション) |
| SIGKILL の後の起動の失敗 | 個体がストリーム中 (0x0002) のまま残り、cmd3 では戻らない | 起動時に `s1::to_idle` で停止。`capture` はリンク鍵を検証した時点で保存 |
| Linux の VM でのコールドブートの失敗 | 起動の割り込みは電源を入れた後に 1 度しか発生しない | 最大 500 ms 待ち、状態ビット (0x90074 の bit 2) が立っていれば続行 |
| WinUSB の制御転送が `ERROR_NOACCESS` (998) で失敗 | `WinUsb_ControlTransfer` は OUT でもバッファーに書き込めるかを検査し、読み取り専用の定数で失敗 | `control_out` はコピーを渡す |
| 衛星の後の地デジが一切ロックしない | 衛星の選局列が地デジ側を待機状態にし、地デジの選局列は復帰させていなかった | `fe1::tune_t` の先頭で地デジ側を復帰させる。選局まわりを変えたら、衛星の後の地デジの選局も試すこと |
| カードの抜き挿しで映像が戻らない。TVTest を開いたまま USB を挿し直しても映らない | ファームウェアはカードのセッションを張り直さない。BonDriver は古いハンドルのまま選局していた | 上の "B-CAS カードと USB の抜き挿し" セクション |

## 未解決の課題

- 保留中: CS ショップチャンネル (ND08、IF 1733 MHz) の視聴中のドロップ
  - 症状: 1 秒から 25 秒の塊で、全 PID の TS が欠ける (1 回に連続性エラーが数個から 1300 個、TEI 付きのパケットは 0)。
    塊の数は時間帯で増減する。
  - 分かっていること: デバイスが USB に出した時点で欠けている (worker の連続性エラーと TVTest のドロップがほぼ同じ数で、FIFO のあふれも無い)。
    無料放送なのでスクランブルでもない。
    塊の間のみ復調の誤りカウンター 0xEB が 0 でなくなり、C/N も下がるので、受信信号の乱れが有力。
    別の系統の 2 か所 (Windows と LXC の個体) で同じ時間帯に起き、同じ系統の ND20 (1973 MHz) では起きない。
  - 疑い: LTE の 1.7 GHz 帯の上り (1710-1785 MHz、夜中に減る) の混入、共通の部分 (V-ONU、幹線、分配器)、放送そのもの
  - 次に確認すること: 2 台で同時に受信して、塊が同じ秒に起きるか。
    スマートフォンを V-ONU や分配器の近くで送信させて再現するか。
  - 手がかり: ログの `s1 stats min N: ...` の行と、停止時の `s1 worker: ...` の行
- ストリームの開始と停止を数十回繰り返すと TS が出なくなることがあった (keepalive などを直す前)。
  今のコードでは再現しない (上の "実機での確認" セクションの「繰り返しの起動」)。
- Linux で確認していないこと: 複数の個体を並べた運用、契約していない有料チャンネルの録画、B-CAS カードの抜き挿し、TS ごとのチャンネルでの EPGStation の録画
- 全サービス: TVTest のスキャン (1 TS 1 行の衛星を含む) は未確認
- BS / CS のロゴ: 全サービスに対応してから 1 度も届いていないので、今のコードで `dt295 logo` と TVTest が受信できるかは確認していない。
  single-service のレジスターの設定は、受信できた版と同じ。
- CATV と UHF 51ch 以上 (700 MHz 以上) は、実機で確認していない。
  静的解析では、周波数で変わるのは 700 MHz 以上の 2 つのレジスターのみ ([device.md](device.md))。

## 方針

- ファームウェアの `.bin` と状態ファイル (デバイスのリンク鍵を含む) はコミットしない (`.gitignore` 済み)
- 第三者から提供された解析を引くときは「第三者から提供された」「第三者提供の」と書き、提供元のツール名やファイル名は書かない
