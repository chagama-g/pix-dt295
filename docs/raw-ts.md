# raw-TS 経路

スクランブルされたままの TS をそのまま配信し、B-CAS の解除をホスト側に任せる経路の、内部のまとめ。
解除先は、Windows (TVTest) では CAS プラグイン、Linux (mirakc) では録画のパイプラインに置いた外部ツール (libarib25 の `b25` など)。
使い方は、Windows が [usage.md](usage.md) の "`BonDriver_DT295.ini`" セクション (ini の `RawTS`)、Linux が同じく "`dt295 serve`" の "raw-TS モード" セクション (`/etc/dt295.conf` の `raw-ts`)。
System1 (視聴) の経路は [development.md](development.md)、デバイスの仕様は [device.md](device.md)。

## 概要

- 通常の System1 経路がデバイスの中でデスクランブルまでするのに対し、raw-TS 経路はスクランブルされたままの TS をホストへ渡し、解除はホスト側に任せる
- どちらで動くかは、Windows は ini の `RawTS` (`1` で raw-TS, `0` 既定で System1)、Linux は `/etc/dt295.conf` の個体の `raw-ts` (`true` で raw-TS, 既定 `false` で System1)
- raw-TS モードは完全に自前コードのみ: レジデント (チップに常駐させる第三者のコード) も補助コア (demod を動かす副 CPU) も純正ファームも使わず、ホストがチップを cold (電源投入直後) から起こし、ホスト面の I2C (`0x00FD8FE0`) で選局し、取り込みエンジンを 0xBC バンク窓で設定して自前の pump で配信する (下の "仕組み")
- 最初の選局だけ、cold の個体が要る (下の "最初のチューニングと再オープン" セクション)

## 機能差 (System1 との違い)

| 項目 | System1 | raw-TS |
|---|---|---|
| デスクランブル | デバイスの中 (内蔵スロットの B-CAS カード) | ホスト側 (Windows は TVTest の CAS プラグイン, Linux は b25 など) + PC/SC リーダーのカード |
| ホストへ出す TS | デスクランブル済み | スクランブルされたまま |
| ファームウェア | 純正の `Firmware` / `TrcFirmware` | 要らない (ホストが cold から立ち上げる) |
| ベンダーコード | 動く (純正ファーム) | 一切動かない (レジデント, 補助コア, 純正ファーム不使用) |
| C/N | worker が毎秒読む実値 | worker が毎秒 ホスト面 I2C で読む実値 (System1 と約 1 dB 以内) |
| 最初の選局 | 速い | cold は立ち上げで遅い (Windows は open で prime, Linux はデーモンが開いたまま保持) |
| B-CAS カードの抜き挿しからの復帰 | ドライバーの `CardWatch` | ホスト側の解除ツール任せ (カードがデバイス外のため) |

## 構成

raw-TS 経路のために、ハードウェアの土台を `dt295-core` に切り出し、自前のオンチップのコードとホスト側を `dt295-rawfw` にまとめた。

| crate | 種類 | 内容 |
|---|---|---|
| `src/dt295-core` | lib | ハードウェアの土台。レジスターとコマンドのプロトコル, I2C, フロントエンド (選局, ロック待ち, C/N), チャンネル表, ファームウェアの転送, 共有する型 (`System` / `Status` / `TsSink`) |
| `src/dt295-rawfw` | lib | 自前コードのみの raw-TS 経路。cold stream の worker, FR monitor のホスト側, ホスト面の demod I2C。`dt295-core` の上に乗る |
| `src/dt295` | lib | System1 の経路とアプリの土台。`dt295-core` と `dt295-rawfw` の公開 API を re-export する |
| `src/dt295-bondriver` | cdylib | `BonDriver_DT295.dll`。raw-TS 経路の呼び出しは `driver.rs` (選局, ストリーム, idle keeper) |

ファームウェアの `.bin` はコミットしない ([development.md](development.md) の "方針" セクション)。

## 仕組み

### モードの選択

ini の `RawTS=1` が `Config::raw_ts` に入ると、`driver.rs` の `RawMode` (マーカー) ができ、選局は raw-TS (cold, 自前コードのみ) 経路へ回る。
最初の選局でチップを立ち上げ (`cold_arm` の `full`)、以降のチャンネル変更は board を立て直さず retune + 取り込み再 arm だけ (立ち上げ済みかはチップ上の monitor を読んで判断する `cold_monitor_present`)。

BonDriver の経路はすべてホスト側で、cold の立ち上げはホストの 0xBC 書き込みとホスト面の I2C だけで通す。

### ストリームの開始 (`SetChannel`)

`set_channel_raw` は cold の自前コード経路 `set_channel_cold` を呼ぶ。流れ:

1. 前の worker を止める。
   raw-TS 経路はデバイスが TS の全サービスを出し、サービスの選択とデスクランブルは TVTest がするので、行ごとの `ServiceID` は使わない。
2. チップに自前の monitor が載っているかを読んで、立ち上げ済みか判断する (`cold_monitor_present`)。cold なら最初の選局で全部立ち上げ、立ち上げ済みなら retune と取り込み再 arm だけで速い。
   フラグではなくチップを読むので、worker の再起動や差し直し無しの再オープンをまたいでも正しく分かる (2 度目の立ち上げは fault するので、載っている monitor を読んで避ける)。
3. 新しい周波数で worker を起こし、FIFO に TS が届くのを待つ (`start_cold_and_wait`)。
4. TS が流れたら前回チャンネルを状態ファイルに残す。流れなければ worker を止める。

成否は worker を持っているデバイスへ触らず、FIFO に TS が届いたかで判断する (下の "worker と FIFO" セクション)。

### worker と FIFO

ストリーム中にデバイスと I/O するのは worker のみ、というのは System1 と同じ。
`Device::stream_start_cold` が worker を立て、worker がデバイスを所有して capture の TS を FIFO (`TsFifo`) へ積む。
ホスト (`GetTsStream`) は FIFO から lock-free に読むだけで、停止のときだけ worker を止める。
読み手が遅れて FIFO があふれたら、古いバイトを捨てて数える ([fifo.rs](../src/dt295-bondriver/src/fifo.rs))。

### 立ち上げと取り込み

レジスターと値は [device.md](device.md) の "raw-TS 経路で使うブロック" セクション。

- 最初の選局: フロントエンドの立ち上げ (I2C コントローラー, I2C のピン, リセットの解除) -> 復調とチューナーの初期化 -> monitor の書き込み
- 選局ごと: 選局してロックを待ち、取り込みを設定して開始
- ストリームの終わり: 取り込みを止める。
  止まっていない (前の worker が異常終了したなど) ときは、次の設定の前に止める

### pump と取りこぼし

チップの capture ring は SDRAM の 0x02000000 からの 4 MiB で、CS の 1 トランスポンダー (約 4.9 MB/s) の約 0.85 秒分が入る。
ring を空けるのは worker が約 2 ms ごとに呼ぶ pump のみで、pump がそれ以上遅れると TS が失われ、ホストには CC の飛びとしてのみ見える。
純正ファームウェアの ring (192 KiB, CS で約 40 ms) では、地デジでも pump が遅れるとあふれる (地デジで 1 秒ごとに 600 ms 止めた試験で、192 KiB は CC の飛び 95 件と約 4 割の欠け、4 MiB は欠け無し)。
そのうえで worker は次のようにして pump を遅らせない。

- ロックしたら、worker のスレッドの優先度を上げる (`prio.rs`)。
  Linux は `SCHED_FIFO` の 1 で、nusb のイベントスレッドも上げる (拒否されたら nice -10)。
  非特権のプロセスは `RLIMIT_RTPRIO` と `RLIMIT_NICE` の範囲でのみ上げられる (systemd の `LimitRTPRIO=1` と `LimitNICE=-10`)。
  これは非力な機器のための設定なので、同梱の unit には書かない ([usage.md](usage.md) の "困ったとき" セクション)。
  Windows は `THREAD_PRIORITY_HIGHEST`。
- ストリーム中の C/N は、C/N のレジスターのみ読む (`cold_cn`)
- Linux (usbfs) は、EP の読み出しを 8 本のみ投げておく (停止時の取り消しの間、同じホストコントローラーの他の個体の制御転送が待たされるため)

worker は届いた TS の CC を PID ごとに確認し、pump の間隔も測る。
ring が満ちる間隔 (約 0.85 秒) 以上はその場で WARN として出力し、欠けがあれば 10 秒ごとにも出力する。
欠けとみなすのは、その間隔があったときと、CC の飛びが壊れたパケット (復調器が直せなかったもの) の数を超えたときで、弱電界だけでは WARN にしない。
集計 (「raw-TS cold: streamed ...」) はストリームの終わりに必ず出力する。

### 自前のコードだけ

運用中も cold の立ち上げも、動くのはすべて自前のコード: boot0 の hook と body (`dt295-frmon` の monitor), ring から EP へ運ぶ pump, capture エンジンの設定 (ホストの 0xBC バンク窓), 復調の tune と retune (`fe1`)。
第三者提供のファームウェアも純正ファームも使わず、1 バイトも動かない。

### 開くときの prime (初回チャンネルの表示)

cold の初回 `SetChannel` は bootstrap で数秒かかり (地デジで約 2.5 秒)、その間 TS が出ない。
TVTest の viewer は初回チャンネルを描画し始められず黒いままになる (チャンネルを変えると直る)。
そこで `OpenTuner` で前もって datapath を bootstrap しておく (`prime_raw`)。
prime 先は前回見ていたチャンネル (状態ファイルの `rawchannel` 行)、無ければ TVTest のスキャン (`.ch2`) の最初の地デジ局。
これで初回 `SetChannel` は速い retune になり、初回チャンネルがそのまま映る。
全体でかかる時間は変わらず、待ちが初回 `SetChannel` から open へ移るだけ。
cold の立ち上げは 1 プラグに 1 回で、prime で monitor が載った後は、選局のたびにチップを読んで立ち上げ済みと分かるので、立ち上げを繰り返さない。

### 開く時間

prime の立ち上げは resident の転送を伴わず、ホストの 0xBC 書き込みとホスト面の I2C だけでチップを cold から起こす。
cold の立ち上げは、フロントエンドの初期化 (約 1 秒) と選局を合わせて地デジで約 2.5 秒かかるが、prime でそれを open に移すので、初回 `SetChannel` は速い retune になる (上の "開くときの prime")。

### 最初のチューニングと再オープン

- 最初のチューニングは cold の個体が要る。
  cold からの立ち上げはホストの 0xBC 書き込みとホスト面の I2C だけで通す (ベンダーのコードは動かない)。
- そのあとは、TVTest を閉じて開き直すだけなら差し直しは要らない。
  monitor はチップに残るので、開き直してもチップを読めば立ち上げ済みと分かり、最初の選局は速い retune になる。
- USB を物理的に差し直すとチップが cold に戻る (RAM が消え、monitor も消える) ので、次の選局で改めて立ち上げる。
  差し直しは物理的な抜き挿し 1 回につき 1 度だけ。

### 止まったときの検出 (idle keeper)

worker は halt した状態でも fault しないので、TS が止まっても自分では気づけない。
idle keeper が raw-TS 用に見張る:

- 最初の TS は最大 `RAW_FIRST_TS` (30 秒) 待つ。
  cold の bootstrap とロック待ち (衛星で最大約 8 秒) より短くすると、その途中で止めてしまい、チューニング前の EP の読み出しキューを壊してチューニングが失敗する。
- 流れ始めた後、`RAW_STALL` (8 秒) の間 1 バイトも届かなければ、データパスが止まったとみなして同じチャンネルを選局し直す (`raw_stall_due`)。
  FIFO の受信バイト数 (`TsFifo::received`) が唯一の生存信号。
- worker が自分で終わっていたら (unplug など) 片付けて、デバイスが抜かれていれば閉じて、戻ってきたら開き直す。

### C/N

worker はストリーム中に約 1 秒ごとに、復調の C/N をホスト面の I2C で読み (`cold_cn` が `fe1::read_cn` を呼ぶ)、デバイスの `Live` に publish する。
`GetSignalLevel` はその実値を返す (`signal_level`)。
3 秒以内のサンプルが無ければ (ストリームが止まった後など) 公称値 `RAW_NOMINAL_CN_DB` (30 dB) に戻る。
System1 経路 (ファームウェア経由で読む) と約 1 dB 以内で一致する。

## Linux (mirakc)

Linux では `dt295 serve` (デーモン) が raw-TS 経路を担う。
BonDriver と同じ `dt295-core` / `dt295-rawfw` の仕組みで、デーモンのスレッドが個体を cold から立ち上げて capture の TS を FIFO に積み、`dt295 tune` のクライアントへ流す (`serve/unit.rs` の `serve_raw`)。

- 個体ごとに `/etc/dt295.conf` の `raw-ts = true` で有効化する
- cold の立ち上げは最初の選局で 1 回。
  デーモンが個体を開いたまま保持するので、以降のチャンネル変更は retune だけ (立ち上げ済みかはチップ上の monitor を読んで判断する。BonDriver と同じ)。
  物理的な抜き挿しで cold に戻る
- B-CAS の解除は mirakc のパイプラインで行う。
  チューナーを `decoded: false` にし、`filters.decode-filter` に外部ツール (libarib25 の `b25 /dev/stdin /dev/stdout` など) を置く。
  解除には PC/SC リーダーの契約済みカードと pcscd が要る
- 止まったときの検出は BonDriver と同じ `raw_stall_due` (最初の TS は最大 30 秒、流れ始めた後は 8 秒無信号で選局し直す)
- EPGStation からの録画と、ブラウザでの視聴まで、純正ファームを使わずに通ることを実機で確認している

## dt295 capture と dt295 logo

`dt295 capture --raw-ts` (と `RawTS=1` の ini、`raw-ts = true` の個体) も同じ経路で、1 回の取り込みごとにプロセスが個体を開いて閉じる。
`dt295 logo` も、`RawTS=1` の ini の個体ではこの経路で受信する。
開始, 最初の TS の待ち, TS が出なかった理由はデーモンと共通 (`rawts.rs`)。

- 立ち上げ済みかはチップ上の monitor で判断するので、別のプロセスからの 2 回目以降も retune だけ (最初の TS まで約 1.7 秒)
- 途中で強制終了 (TerminateProcess) されても、次のプロセスは retune で流し始める
- worker が自分で終わったときと 8 秒無信号のときは選局し直し、抜かれたときは終了する
- capture の worker は TS を queue に入れるだけで、ファイルやパイプへの書き出しは別のスレッド (チップのリングは 1 秒分ほどなので、書き出しを待つと TS が途切れる)
