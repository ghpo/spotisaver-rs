import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// Ícone na barra que abre um painel para colar um link do Spotify, escolher a
// pasta e acompanhar o download com porcentagem ao vivo.
Panel {
  id: root
  moduleName: "ghpo.spotisaver"
  ipcTarget: "ghpo.spotisaver"
  manageIpc: false

  // ---- Configuração do plugin -------------------------------------------
  readonly property string iconGlyph: String(setting("icon", "󰓇"))
  readonly property string binary: {
    var value = String(setting("binary", "spotisaver")).trim()
    return value === "" ? "spotisaver" : value
  }
  readonly property string audioFormat: {
    var value = String(setting("format", "mp3")).trim()
    return value === "" ? "mp3" : value
  }
  readonly property int jobCount: {
    var value = Number(setting("jobs", 3))
    if (!isFinite(value)) return 3
    return Math.max(1, Math.min(8, Math.round(value)))
  }
  property string dir: String(setting("downloadDir", ""))
  onSettingsChanged: if (!root.downloading) root.dir = String(root.setting("downloadDir", ""))

  // ---- Cores/tema -------------------------------------------------------
  readonly property color foreground: bar ? bar.foreground : Commons.Color.foreground
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property color accent: Commons.Color.accent
  readonly property color urgent: Commons.Color.urgent
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property color trackFill: Qt.rgba(foreground.r, foreground.g, foreground.b, 0.14)

  // ---- Estado -----------------------------------------------------------
  property string url: ""
  property bool downloading: false
  property bool choosingDir: false
  property bool canceling: false
  property bool finished: false
  property bool failed: false
  property bool doneSeen: false
  property bool autoStartAfterPick: false
  property string notice: ""
  property string errorText: ""
  property string collectionName: ""
  property int total: 0
  property int downloaded: 0
  property int skipped: 0
  property int failures: 0
  property real percent: 0
  property string currentArtist: ""
  property string currentTitle: ""
  property var activeCommand: []

  readonly property bool hasLink: looksLikeSpotifyLink(url)
  readonly property bool busy: downloading || choosingDir
  readonly property string displayPercent: Math.round(percent) + "%"
  readonly property string folderLabel: {
    var value = String(root.dir || "").trim()
    if (value === "") return "Nenhuma pasta escolhida"
    var home = Quickshell.env("HOME")
    if (home && value === home) return "~"
    if (home && value.indexOf(home + "/") === 0) return "~" + value.slice(home.length)
    return value
  }

  // ---- Utilidades -------------------------------------------------------
  function expandHome(value) {
    var text = String(value || "")
    if (text === "~") return Quickshell.env("HOME")
    if (text.indexOf("~/") === 0) return Quickshell.env("HOME") + text.slice(1)
    return text
  }

  function looksLikeSpotifyLink(value) {
    var text = String(value || "").trim()
    if (text === "") return false
    if (/^spotify:(playlist|track):[A-Za-z0-9]+$/.test(text)) return true
    return /^https?:\/\/open\.spotify\.com\/(intl-[A-Za-z-]+\/)?(playlist|track)\/[A-Za-z0-9]+/.test(text)
  }

  function focusUrl() {
    if (panel.loadedContent && panel.loadedContent.urlField)
      panel.loadedContent.urlField.forceActiveFocus()
  }

  function notify(headline, body) {
    Quickshell.execDetached([
      "omarchy-notification-send",
      "--app-name", "Spotisaver",
      "-g", root.iconGlyph,
      String(headline),
      String(body)
    ])
  }

  function persistDir(path) {
    if (!root.bar || !root.bar.shell || typeof root.bar.shell.updateEntryInline !== "function") return
    var entry = { id: root.moduleName }
    for (var key in root.settings) if (key !== "id") entry[key] = root.settings[key]
    entry.downloadDir = path
    root.bar.shell.updateEntryInline(root.moduleName, entry)
  }

  function openFolder() {
    var target = root.expandHome(root.dir)
    if (target !== "") Quickshell.execDetached(["xdg-open", target])
  }

  function resetResult() {
    root.finished = false
    root.failed = false
    root.doneSeen = false
    root.errorText = ""
    root.collectionName = ""
    root.total = 0
    root.downloaded = 0
    root.skipped = 0
    root.failures = 0
    root.percent = 0
    root.currentArtist = ""
    root.currentTitle = ""
  }

  // ---- Escolha da pasta -------------------------------------------------
  function chooseDir(autoStart) {
    if (root.choosingDir) return
    root.autoStartAfterPick = autoStart === true
    root.choosingDir = true
    var start = String(root.dir || "").trim()
    start = start === "" ? root.expandHome("~/Music") : root.expandHome(start)
    if (start.charAt(start.length - 1) !== "/") start += "/"
    // O seletor é uma janela comum; fechar o painel solta o foco do teclado
    // para o zenity receber o que for digitado.
    root.close()
    Qt.callLater(function() {
      dirProcess.command = [
        "zenity",
        "--file-selection",
        "--directory",
        "--title=Escolha a pasta de download",
        "--filename=" + start
      ]
      dirProcess.running = true
    })
  }

  function finishDirPick(path) {
    root.choosingDir = false
    var chosen = String(path || "").trim()
    var auto = root.autoStartAfterPick
    root.autoStartAfterPick = false
    if (chosen !== "") {
      root.dir = chosen
      root.persistDir(chosen)
      root.notice = ""
    }
    root.open()
    if (auto && chosen !== "") {
      Qt.callLater(root.beginDownload)
      return
    }
    Qt.callLater(root.focusUrl)
  }

  // ---- Download ---------------------------------------------------------
  function beginDownload() {
    if (root.downloading) return
    var link = root.url.trim()
    if (!root.looksLikeSpotifyLink(link)) {
      root.notice = "Cole um link de playlist ou música do Spotify"
      root.focusUrl()
      return
    }
    if (String(root.dir || "").trim() === "") {
      root.chooseDir(true)
      return
    }

    root.resetResult()
    root.notice = ""
    root.downloading = true
    root.activeCommand = [
      root.expandHome(root.binary),
      link,
      "--download", root.expandHome(root.dir),
      "--format", root.audioFormat,
      "--jobs", String(root.jobCount),
      "--json-progress"
    ]
    downloadProc.running = true
  }

  function cancelDownload() {
    if (!downloadProc.running) return
    root.canceling = true
    downloadProc.signal(15)
  }

  function completeSuccess() {
    root.doneSeen = true
    root.finished = true
    root.failed = false
    root.percent = 100
    var parts = [root.downloaded + " baixada(s)"]
    if (root.skipped > 0) parts.push(root.skipped + " pulada(s)")
    if (root.failures > 0) parts.push(root.failures + " falha(s)")
    root.notice = parts.join(" · ")
    root.notify(root.collectionName !== "" ? root.collectionName : "Download concluído", root.notice)
  }

  function handleEvent(line) {
    var event
    try {
      event = JSON.parse(String(line))
    } catch (e) {
      return
    }
    if (!event || typeof event !== "object") return

    var kind = String(event.event || "")
    if (kind === "start") {
      root.collectionName = String(event.name || "")
      root.total = Number(event.total) || 0
    } else if (kind === "track") {
      var status = String(event.status || "")
      if (status === "downloading") {
        root.currentArtist = String(event.artist || "")
        root.currentTitle = String(event.title || "")
      } else if (status === "done") {
        root.downloaded += 1
      } else if (status === "skipped") {
        root.skipped += 1
      } else if (status === "failed") {
        root.failures += 1
      }
    } else if (kind === "progress") {
      var value = Number(event.percent)
      if (isFinite(value)) root.percent = Math.max(0, Math.min(100, value))
    } else if (kind === "done") {
      root.downloaded = Number(event.downloaded) || 0
      root.skipped = Number(event.skipped) || 0
      root.failures = Number(event.failed) || 0
      root.percent = 100
      root.completeSuccess()
    }
  }

  function handleExit(exitCode) {
    if (root.canceling) {
      root.canceling = false
      root.downloading = false
      root.finished = false
      root.failed = false
      root.percent = 0
      root.notice = "Download cancelado"
      return
    }

    root.downloading = false
    if (exitCode !== 0) {
      root.failed = true
      root.finished = false
      root.notice = root.errorText !== "" ? root.errorText : "Falha ao baixar"
      root.notify("Spotisaver falhou", root.notice)
      return
    }

    if (!root.doneSeen) {
      root.percent = 100
      root.completeSuccess()
    }
  }

  onOpenedChanged: if (opened) Qt.callLater(root.focusUrl)

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  ShellIpc {
    target: "ghpo.spotisaver"


    function open() { root.open() }
    function close() { root.close() }
    function show() { root.open() }
    function hide() { root.close() }
    function toggle() { root.toggle() }
    function download(url: string): string {
      root.url = String(url || "")
      root.beginDownload()
      return "ok"
    }
  }

  Process {
    id: dirProcess
    running: false
    command: []
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.finishDirPick(String(text || ""))
    }
    onExited: function(exitCode) {
      if (exitCode !== 0) root.finishDirPick("")
    }
  }

  Process {
    id: downloadProc
    running: false
    command: root.activeCommand
    stdout: SplitParser {
      onRead: function(line) { root.handleEvent(line) }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.errorText = String(text || "").trim()
    }
    onExited: function(exitCode) { root.handleExit(exitCode) }
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.iconGlyph
    labelText: root.downloading && !root.vertical ? root.displayPercent : ""
    slotSize: Style.bar.iconSlot * (root.downloading && !root.vertical ? 2 : 1)
    fontSize: Style.font.caption
    tooltipText: root.downloading ? "Spotisaver — " + root.displayPercent : "Spotisaver"
    onPressed: function(buttonCode) {
      if (buttonCode === Qt.LeftButton) root.toggle()
    }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: panel.loadedContent ? (panel.loadedContent.urlField || panel.loadedContent) : null
    contentWidth: panel.fittedContentWidth(Style.space(400))
    contentHeight: panel.fittedContentHeight(panel.loadedContentHeight, Style.space(560))

    content: PanelKeyCatcher {
      id: keyCatcher
      property alias urlField: urlField
      property alias column: column
      anchors.fill: parent
      implicitHeight: column.implicitHeight
      blocked: urlField.activeFocus
      onCloseRequested: root.close()
      onActivateRequested: if (!root.busy) root.beginDownload()

      Component.onCompleted: if (root.opened) Qt.callLater(urlField.forceActiveFocus)

      Connections {
        target: root
        function onOpenedChanged() {
          if (root.opened) Qt.callLater(urlField.forceActiveFocus)
        }
      }

      Column {
        id: column
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        spacing: Style.space(14)

        PanelHero {
          id: hero
          width: parent.width
          title: root.downloading && root.collectionName !== "" ? root.collectionName : "Spotisaver"
          meta: {
            if (root.downloading) return "Baixando · " + root.displayPercent
            if (root.finished) return root.failures > 0 ? "Concluído com falhas" : "Concluído"
            if (root.failed) return "Falhou"
            return "Playlist ou música do Spotify"
          }
          detail: root.downloading ? root.displayPercent : ""
          foreground: root.foreground
          fontFamily: root.fontFamily
          iconComponent: Component {
            Text {
              text: root.iconGlyph
              textFormat: Text.PlainText
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.display
            }
          }
        }

        TextField {
          id: urlField
          width: parent.width
          placeholderText: "Cole o link do Spotify (Ctrl+V)..."
          text: root.url
          foreground: root.foreground
          onTextEdited: {
            root.url = text
            root.notice = ""
          }
          onAccepted: root.beginDownload()
          Keys.onEscapePressed: root.close()
        }

        Text {
          width: parent.width
          visible: root.notice !== "" && !root.downloading && !root.finished && !root.failed
          text: root.notice
          textFormat: Text.PlainText
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          wrapMode: Text.WordWrap
        }

        Row {
          width: parent.width
          spacing: Style.space(8)

          Column {
            width: Math.max(0, parent.width - changeDirButton.width - parent.spacing)
            spacing: Style.space(2)

            Text {
              text: "Pasta de download"
              textFormat: Text.PlainText
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.bold: true
              font.letterSpacing: 1.1
            }

            Text {
              width: parent.width
              text: root.folderLabel
              textFormat: Text.PlainText
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              elide: Text.ElideMiddle
            }
          }

          Button {
            id: changeDirButton
            text: "Trocar"
            bordered: true
            onClicked: root.chooseDir(false)
          }
        }

        Text {
          width: parent.width
          text: root.audioFormat.toUpperCase() + " · " + root.jobCount
            + (root.jobCount === 1 ? " download simultâneo" : " downloads simultâneos")
          textFormat: Text.PlainText
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
        }

        Button {
          width: parent.width
          text: "Baixar"
          visible: !root.downloading
          enabled: root.hasLink
          focusable: true
          bordered: true
          onClicked: root.beginDownload()
        }

        Column {
          width: parent.width
          spacing: Style.space(8)
          visible: root.downloading

          Row {
            width: parent.width

            Text {
              id: percentText
              text: root.displayPercent
              textFormat: Text.PlainText
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.title
              font.bold: true
            }

            Item {
              width: Math.max(0, parent.width - percentText.width - countText.width)
              height: 1
            }

            Text {
              id: countText
              text: root.total > 0
                ? (root.downloaded + root.skipped) + " de " + root.total
                : ""
              textFormat: Text.PlainText
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }
          }

          Rectangle {
            width: parent.width
            height: Style.space(6)
            radius: height / 2
            color: root.trackFill

            Rectangle {
              width: parent.width * Math.max(0, Math.min(1, root.percent / 100))
              height: parent.height
              radius: parent.radius
              color: root.accent
              Behavior on width {
                NumberAnimation {
                  duration: Style.duration(120)
                  easing.type: Easing.OutCubic
                }
              }
            }
          }

          Text {
            width: parent.width
            text: root.currentArtist !== ""
              ? root.currentArtist + " — " + root.currentTitle
              : "Preparando download..."
            textFormat: Text.PlainText
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            elide: Text.ElideRight
          }

          Button {
            text: "Cancelar"
            bordered: true
            onClicked: root.cancelDownload()
          }
        }

        Column {
          width: parent.width
          spacing: Style.space(8)
          visible: root.finished || root.failed

          Text {
            width: parent.width
            text: root.notice
            textFormat: Text.PlainText
            color: root.failed ? root.urgent : root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            wrapMode: Text.WordWrap
          }

          Row {
            spacing: Style.space(8)

            Button {
              text: "Abrir pasta"
              bordered: true
              onClicked: root.openFolder()
            }

            Button {
              text: "Baixar outra"
              bordered: true
              onClicked: {
                root.resetResult()
                root.notice = ""
                root.url = ""
                Qt.callLater(root.focusUrl)
              }
            }
          }
        }
      }
    }
  }
}
