import SwiftUI
import UIKit
import AVFoundation

/// Экран сканера QR (камера) — по коду вызывает onCode и закрывается.
struct ScannerSheet: View {
    @Environment(\.dismiss) var dismiss
    let onCode: (String) -> Void
    /// Разрешена ли камера. nil — ещё спрашиваем систему.
    @State private var allowed: Bool? = nil

    var body: some View {
        NavigationStack {
            ZStack {
                switch allowed {
                case true:
                    QRScannerView { code in onCode(code); dismiss() }.ignoresSafeArea()
                    RoundedRectangle(cornerRadius: 20).stroke(Color.white.opacity(0.9), lineWidth: 3)
                        .frame(width: 230, height: 230)
                    VStack { Spacer(); Text("Наведите камеру на QR приглашения")
                        .foregroundColor(.white).font(.system(size: 14, weight: .medium))
                        .padding(10).background(.black.opacity(0.5)).cornerRadius(10).padding(.bottom, 60) }
                case false:
                    denied
                case nil:
                    ProgressView().tint(.white)
                }
            }
            .background(Color.black)
            .navigationTitle("Сканировать QR").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Отмена") { dismiss() } } }
        }
        .task { allowed = await CameraAccess.request() }
    }

    /// Отказ в доступе к камере. Раньше здесь просто оставался ЧЁРНЫЙ ЭКРАН с
    /// подписью «наведите камеру» — человек считал, что приложение сломалось.
    /// На Android этот случай подписан, и здесь тоже должен быть: объясняем, что
    /// произошло, и ведём прямо в нужный экран настроек — искать его руками в
    /// длинном списке не дело.
    private var denied: some View {
        VStack(spacing: 14) {
            Image(systemName: "camera.fill").font(.system(size: 44)).foregroundColor(.white.opacity(0.5))
            Text("Нет доступа к камере").foregroundColor(.white).font(.system(size: 18, weight: .bold))
            Text("Разрешите камеру в настройках — или введите код сети вручную.")
                .foregroundColor(.white.opacity(0.7)).font(.system(size: 14))
                .multilineTextAlignment(.center).padding(.horizontal, 32)
            if let url = URL(string: UIApplication.openSettingsURLString) {
                Button("Открыть настройки") { UIApplication.shared.open(url) }
                    .font(.system(size: 15, weight: .semibold))
                    .padding(.horizontal, 20).padding(.vertical, 10)
                    .background(Color.white.opacity(0.15)).foregroundColor(.white).cornerRadius(12)
            }
        }
    }
}

/// Спросить разрешение на камеру и дождаться ответа.
enum CameraAccess {
    static func request() async -> Bool {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: return true
        case .notDetermined: return await AVCaptureDevice.requestAccess(for: .video)
        default: return false   // denied / restricted — переспрашивать система не даст
        }
    }
}

struct QRScannerView: UIViewControllerRepresentable {
    let onCode: (String) -> Void
    func makeUIViewController(context: Context) -> ScannerVC { let vc = ScannerVC(); vc.onCode = onCode; return vc }
    func updateUIViewController(_ vc: ScannerVC, context: Context) {}
}

final class ScannerVC: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var onCode: ((String) -> Void)?
    private let session = AVCaptureSession()
    private var preview: AVCaptureVideoPreviewLayer?
    private var handled = false

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        guard let device = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: device),
              session.canAddInput(input) else { return }
        session.addInput(input)
        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else { return }
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]
        let p = AVCaptureVideoPreviewLayer(session: session)
        p.videoGravity = .resizeAspectFill
        p.frame = view.layer.bounds
        view.layer.addSublayer(p)
        preview = p
        DispatchQueue.global(qos: .userInitiated).async { self.session.startRunning() }
    }

    override func viewDidLayoutSubviews() { super.viewDidLayoutSubviews(); preview?.frame = view.layer.bounds }
    override func viewWillDisappear(_ animated: Bool) { super.viewWillDisappear(animated); if session.isRunning { session.stopRunning() } }

    func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput objs: [AVMetadataObject], from connection: AVCaptureConnection) {
        guard !handled, let obj = objs.first as? AVMetadataMachineReadableCodeObject, let s = obj.stringValue else { return }
        handled = true
        onCode?(s)
    }
}
