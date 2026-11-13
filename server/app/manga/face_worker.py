"""Optional external Python worker. No heavy ML imports in Velmira's reader process."""
import json
import sys
from pathlib import Path


def main():
    import cv2
    import numpy as np
    import onnxruntime as ort
    from insightface.model_zoo.scrfd import SCRFD
    from insightface.model_zoo.arcface_onnx import ArcFaceONNX
    from insightface.utils import face_align

    args = json.loads(sys.argv[1])
    root = Path(args["models"])
    opts = ort.SessionOptions(); opts.intra_op_num_threads = 2; opts.inter_op_num_threads = 1
    detector = root / "det_10g.onnx"; recognizer = root / "w600k_r50.onnx"
    if not detector.is_file() or not recognizer.is_file():
        raise ValueError("缺少 buffalo_l 的 det_10g.onnx / w600k_r50.onnx")
    det = SCRFD(model_file=str(detector), session=ort.InferenceSession(str(detector), opts, providers=["CPUExecutionProvider"]))
    rec = ArcFaceONNX(model_file=str(recognizer), session=ort.InferenceSession(str(recognizer), opts, providers=["CPUExecutionProvider"]))
    det.prepare(-1, input_size=(640, 640)); rec.prepare(-1)
    vectors = []
    for path in [args["reference"], args["image"]]:
        image = cv2.imdecode(np.fromfile(path, dtype=np.uint8), cv2.IMREAD_COLOR)
        boxes, points = det.detect(image, threshold=.45, max_num=0)
        if len(boxes) != 1:
            print(json.dumps({"status": "needs_review", "reason": "no_face" if len(boxes) == 0 else "multiple_faces", "face_count": len(boxes)})); return
        face = face_align.norm_crop(image, landmark=points[0])
        vector = rec.get_feat(face).flatten()
        norm = np.linalg.norm(vector)
        if not np.isfinite(norm) or norm < 1e-9:
            raise ValueError("人脸特征无效")
        vectors.append(vector / norm)
    score = float(np.dot(*vectors))
    print(json.dumps({"status": "measured", "score": score, "model": "buffalo_l/ArcFace", "face_count": 1}))


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(json.dumps({"status": "unavailable", "reason": str(exc)}))
