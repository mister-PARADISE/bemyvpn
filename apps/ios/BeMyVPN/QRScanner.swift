import SwiftUI
import AVFoundation

/// Экран сканера QR (камера) — по коду вызывает onCode и закрывается.
struct ScannerSheet: View {
    @Environment(\.dismiss) var dismiss
    let onCode: (String) -> Void
    var body: some View {
        NavigationStack {
            ZStack {
                QRScannerView { code in onCode(code); dismiss() }.ignoresSafeArea()
                RoundedRectangle(cornerRadius: 20).stroke(Color.white.opacity(0.9), lineWidth: 3)
                    .frame(width: 230, height: 230)
                VStack { Spacer(); Text("Наведите камеру на QR приглашения")
                    .foregroundColor(.white).font(.system(size: 14, weight: .medium))
                    .padding(10).background(.black.opacity(0.5)).cornerRadius(10).padding(.bottom, 60) }
            }
            .background(Color.black)
            .navigationTitle("Сканировать QR").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Отмена") { dismiss() } } }
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
