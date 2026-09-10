// Independent reference executable: does not include the SDK's Rust or C shim.
// Usage: mnn-reference decoded-197 decoded-198 reference.bin
#include <MNN/Interpreter.hpp>
#include <MNN/Tensor.hpp>
#include <array>
#include <cstdint>
#include <cstring>
#include <fstream>
#include <iostream>
#include <memory>
#include <stdexcept>
#include <vector>

static float sample(int model, int input, size_t index, int pattern) {
    if (model == 198)
        return (static_cast<int>((index * (pattern == 0 ? 17 : 71) + 23 + pattern * 37) % 257) - 128) / 128.0f;
    const int factor = input == 0 ? pattern + 3 : input == 1 ? pattern + 7 : 13;
    const int offset = input == 0 ? 19 : input == 1 ? 43 : pattern * 31;
    return static_cast<float>((index * factor + offset) % 257) / 256.0f;
}

int main(int argc, char** argv) {
    try {
        if (argc != 4 || std::strcmp(MNN::getVersion(), "3.6.1") != 0)
            throw std::runtime_error("requires decoded model paths and linked MNN 3.6.1");
        std::ofstream output(argv[3], std::ios::binary);
        output.write("MNNREF1\0", 8);
        for (int model : {197, 198}) {
            std::unique_ptr<MNN::Interpreter> interpreter(MNN::Interpreter::createFromFile(argv[model == 197 ? 1 : 2]));
            if (!interpreter) throw std::runtime_error("model open failed");
            MNN::BackendConfig backend;
            backend.precision = MNN::BackendConfig::Precision_High;
            MNN::ScheduleConfig config;
            config.type = MNN_FORWARD_CPU;
            config.numThread = 1;
            config.backendConfig = &backend;
            auto* session = interpreter->createSession(config);
            if (!session) throw std::runtime_error("session open failed");
            const std::vector<const char*> names = model == 197
                ? std::vector<const char*>{"ctt_h_img", "ctt_l_img", "sty_l_img"}
                : std::vector<const char*>{"input"};
            for (int pattern = 0; pattern < 2; ++pattern) {
                for (size_t input = 0; input < names.size(); ++input) {
                    auto* tensor = interpreter->getSessionInput(session, names[input]);
                    if (!tensor) throw std::runtime_error("input missing");
                    MNN::Tensor host(tensor, MNN::Tensor::CAFFE);
                    for (int index = 0; index < host.elementSize(); ++index)
                        host.host<float>()[index] = sample(model, input, index, pattern);
                    if (!tensor->copyFromHostTensor(&host)) throw std::runtime_error("input copy failed");
                }
                if (interpreter->runSession(session) != MNN::NO_ERROR) throw std::runtime_error("inference failed");
                auto* tensor = interpreter->getSessionOutput(session, model == 197 ? "output" : "avgpool");
                if (!tensor) throw std::runtime_error("output missing");
                MNN::Tensor host(tensor, MNN::Tensor::CAFFE);
                if (!tensor->copyToHostTensor(&host)) throw std::runtime_error("output copy failed");
                const int expected = model == 197 ? 3 * 17 * 17 * 17 : 576;
                if (host.elementSize() != expected) throw std::runtime_error("output shape changed");
                // Explicit little-endian float bits keep the fixture portable.
                for (int index = 0; index < expected; ++index) {
                    uint32_t bits;
                    std::memcpy(&bits, host.host<float>() + index, sizeof(bits));
                    for (int byte = 0; byte < 4; ++byte) output.put(static_cast<char>(bits >> (byte * 8)));
                }
            }
        }
        if (!output.good()) throw std::runtime_error("reference write failed");
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
