"""MediaPipe's selfie segmenter (TFLite, NHWC, a custom transposed convolution) as an ONNX graph in NCHW: the
person segmentation model the camera's background effects run (crates/teams-media/src/video/effects.rs), on tract.

The source is Google's `selfie_segmenter_landscape` (float16, Apache-2.0, the model card's licence):
https://storage.googleapis.com/mediapipe-models/image_segmenter/selfie_segmenter_landscape/float16/latest/selfie_segmenter_landscape.tflite
(sha256 490e9ea734313e0de10fa0cd9e3c6133e36ea4db2b7a49bde9ef019f72796b8e). Its weights are only widened to float32 and
laid out for NCHW; every op maps to its ONNX twin, the custom `Convolution2DTransposeBias` to `ConvTranspose`. The
graph takes `input` [1, 3, 144, 256] (RGB in [0, 1]) and gives `mask` [1, 1, 144, 256]: the person's confidence.

Usage: python3 tools/selfie-segmenter/convert.py selfie_segmenter_landscape.tflite \
    crates/teams-media/assets/segmentation/selfie_segmenter_landscape.onnx   (numpy, onnx and tflite)
"""
import struct, sys
import numpy as np
import onnx
import tflite
from onnx import TensorProto, helper, numpy_helper

src, dst = sys.argv[1], sys.argv[2]
buf = open(src, 'rb').read()
m = tflite.Model.GetRootAsModel(buf, 0)
g = m.Subgraphs(0)
names = {v: k for k, v in tflite.BuiltinOperator.__dict__.items() if not k.startswith('_')}

DT = {0: np.float32, 1: np.float16, 2: np.int32}


def tensor(i):
    return g.Tensors(i)


def tname(i):
    return 't%d' % i


def shape(i):
    return [int(x) for x in tensor(i).ShapeAsNumpy()]


def raw(i):
    t = tensor(i)
    b = m.Buffers(t.Buffer())
    if b.DataLength() == 0:
        return None
    return np.frombuffer(b.DataAsNumpy().tobytes(), dtype=DT[t.Type()]).reshape(shape(i) or [-1])


consts = {}  # tensor index -> float32 numpy (NHWC/tflite layout)
nodes, inits = [], []
counter = [0]


def fresh(base):
    counter[0] += 1
    return '%s_%d' % (base, counter[0])


def const(i):
    if i in consts:
        return consts[i]
    r = raw(i)
    if r is None:
        raise KeyError(i)
    return r


def init(arr, base):
    n = fresh(base)
    inits.append(numpy_helper.from_array(np.ascontiguousarray(arr), n))
    return n


def same_pads(in_hw, k_hw, s_hw):
    pads_b, pads_e = [], []
    for n, k, s in zip(in_hw, k_hw, s_hw):
        out = -(-n // s)
        total = max((out - 1) * s + k - n, 0)
        pads_b.append(total // 2)
        pads_e.append(total - total // 2)
    return pads_b + pads_e


def activation(out, act):
    if act == 0:
        return out
    pre = fresh('preact')
    if act == 1:
        nodes.append(helper.make_node('Relu', [pre], [out]))
    elif act == 3:
        nodes.append(helper.make_node('Clip', [pre, init(np.float32(0), 'zero'), init(np.float32(6), 'six')], [out]))
    else:
        raise ValueError('activation %d' % act)
    return pre


for k in range(g.OperatorsLength()):
    op = g.Operators(k)
    c = m.OperatorCodes(op.OpcodeIndex())
    code = max(c.BuiltinCode(), c.DeprecatedBuiltinCode())
    name = names.get(code) if code != 32 else 'CUSTOM:' + c.CustomCode().decode()
    ins = list(op.InputsAsNumpy())
    outs = list(op.OutputsAsNumpy())
    out = tname(outs[0])
    t = op.BuiltinOptions()
    if name == 'DEQUANTIZE':
        consts[outs[0]] = raw(ins[0]).astype(np.float32)
    elif name in ('CONV_2D', 'DEPTHWISE_CONV_2D'):
        o = (tflite.Conv2DOptions() if name == 'CONV_2D' else tflite.DepthwiseConv2DOptions())
        o.Init(t.Bytes, t.Pos)
        assert o.DilationHFactor() == 1 and o.DilationWFactor() == 1
        w = const(ins[1])
        b = const(ins[2])
        n, h, wd, cin = shape(ins[0])
        if name == 'CONV_2D':
            weight = w.transpose(0, 3, 1, 2)
            group = 1
        else:
            weight = w.transpose(3, 0, 1, 2)
            group = cin
        kh, kw = weight.shape[2], weight.shape[3]
        s = [o.StrideH(), o.StrideW()]
        pads = same_pads([h, wd], [kh, kw], s) if o.Padding() == 0 else [0, 0, 0, 0]
        target = activation(out, o.FusedActivationFunction())
        nodes.append(helper.make_node('Conv', [tname(ins[0]), init(weight, 'w'), init(b, 'b')], [target],
                                      kernel_shape=[kh, kw], strides=s, pads=pads, group=group))
    elif name == 'HARD_SWISH':
        hs = fresh('hsig')
        nodes.append(helper.make_node('HardSigmoid', [tname(ins[0])], [hs], alpha=1.0 / 6.0, beta=0.5))
        nodes.append(helper.make_node('Mul', [tname(ins[0]), hs], [out]))
    elif name == 'RELU':
        nodes.append(helper.make_node('Relu', [tname(ins[0])], [out]))
    elif name == 'LOGISTIC':
        nodes.append(helper.make_node('Sigmoid', [tname(ins[0])], [out]))
    elif name == 'MEAN':
        axes = list(const(ins[1]))
        assert sorted(axes) == [1, 2], axes
        nodes.append(helper.make_node('GlobalAveragePool', [tname(ins[0])], [out]))
    elif name in ('ADD', 'MUL'):
        act = 0
        if t:
            o = tflite.AddOptions() if name == 'ADD' else tflite.MulOptions()
            o.Init(t.Bytes, t.Pos)
            act = o.FusedActivationFunction()
        for i in ins:
            assert raw(i) is None and i not in consts, 'constant operand'
        target = activation(out, act)
        nodes.append(helper.make_node('Add' if name == 'ADD' else 'Mul', [tname(i) for i in ins], [target]))
    elif name == 'RESIZE_BILINEAR':
        o = tflite.ResizeBilinearOptions()
        o.Init(t.Bytes, t.Pos)
        assert o.HalfPixelCenters() and not o.AlignCorners()
        size = list(const(ins[1]))
        n, h, wd, cin = shape(ins[0])
        sizes = np.array([n, cin, size[0], size[1]], dtype=np.int64)
        nodes.append(helper.make_node('Resize', [tname(ins[0]), '', '', init(sizes, 'sizes')], [out],
                                      mode='linear', coordinate_transformation_mode='half_pixel'))
    elif name == 'CUSTOM:Convolution2DTransposeBias':
        padding, stride_w, stride_h = struct.unpack('<iii', bytes(op.CustomOptionsAsNumpy()))
        w = const(ins[1])  # O H W I
        b = const(ins[2])
        weight = w.transpose(3, 0, 1, 2)  # I O H W
        kh, kw = weight.shape[2], weight.shape[3]
        assert padding == 1 and (kh, kw) == (stride_h, stride_w), (padding, kh, kw, stride_h, stride_w)
        nodes.append(helper.make_node('ConvTranspose', [tname(ins[0]), init(weight, 'w'), init(b, 'b')], [out],
                                      kernel_shape=[kh, kw], strides=[stride_h, stride_w]))
    else:
        raise ValueError('unsupported op ' + str(name))

inp = g.Inputs(0)
outp = g.Outputs(0)
n, h, w, ch = shape(inp)
graph = helper.make_graph(
    nodes, 'selfie_segmenter',
    [helper.make_tensor_value_info(tname(inp), TensorProto.FLOAT, [1, ch, h, w])],
    [helper.make_tensor_value_info(tname(outp), TensorProto.FLOAT, [1, 1, h, w])],
    inits)
model = helper.make_model(graph, opset_imports=[helper.make_opsetid('', 13)], producer_name='ngteams')
model.ir_version = 8
onnx.checker.check_model(model)
# rename input/output to stable names
for node in model.graph.node:
    node.input[:] = ['input' if x == tname(inp) else x for x in node.input]
    node.output[:] = ['mask' if x == tname(outp) else x for x in node.output]
model.graph.input[0].name = 'input'
model.graph.output[0].name = 'mask'
onnx.checker.check_model(model)
onnx.save(model, dst)
print('saved', dst, len(nodes), 'nodes')
