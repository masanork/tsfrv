tsrfv: Telnet over SSL/TLS 電子公告ビューア
===

概要
---

本プログラムはTelnet over SSL/TLSプロトコルで提供される電子公告を、コンソールで閲覧・保存するプログラムです。コマンドライン引数なしで立ち上げた場合、デフォルトで[「一般社団法人サイバー技術・インターネット自由研究会」の電子公告システム](telnet://koukoku.shadan.open.ad.jp)を表示します。コマンドライン引数でその他のサーバーやポート番号を指定した場合、通常のtelnetsクライアントと同様に、指定されたサイトに接続します。
5秒間、電子公告システムからのパケットが途絶えた場合、公告が終了したものと判断して、TCPセッション遮断時点の日時YYYYMMDDHHMM.txtというファイル名でログを保存してプログラムを終了します。

使い方
---

「一般社団法人サイバー技術・インターネット自由研究会」の電子公告システムを表示する場合

``` bash
tsfrv
```

その他のtelnetサーバーに接続する場合

``` bash
tsfrv [サーバー名] [ポート番号]
```

残課題
---

- できればtelnet版tfrvとソースコードを統合したい。
- セッションを終了した時間の日時をファイル名に含めているが、セッション開始時点の日時も含めてファイルに保存すべきか。
- チャットモードで書込があった場合、公示と異なる内容がログに残ってしまうが、公示とチャットを分離してログとして保存できないか検討。

ライセンス
---

MIT License