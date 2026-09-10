// Private C ABI around the independently built MNN 3.6.1 CPU interpreter.
// No Insta360 executable code is loaded or linked.
#include <MNN/Interpreter.hpp>
#include <MNN/Tensor.hpp>
#include <algorithm>
#include <cmath>
#include <cstddef>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

namespace {
struct TensorBuffer {
    MNN::Tensor* device;
    std::unique_ptr<MNN::Tensor> host;
    TensorBuffer(MNN::Tensor* tensor) : device(tensor) {
        if (!tensor || tensor->getType().code != halide_type_float || tensor->getType().bits != 32)
            throw std::runtime_error("MNN model has an unexpected tensor type");
        host.reset(new MNN::Tensor(tensor, MNN::Tensor::CAFFE));
    }
};
struct Model {
    std::unique_ptr<MNN::Interpreter> interpreter;
    MNN::Session* session = nullptr;
    std::vector<TensorBuffer> inputs;
    TensorBuffer output;
    static MNN::Tensor* initialize(Model* self, const void* bytes, size_t length,
                                  const char* output_name) {
        self->interpreter.reset(MNN::Interpreter::createFromBuffer(bytes, length));
        if (!self->interpreter) throw std::runtime_error("MNN rejected model data");
        MNN::BackendConfig backend;
        backend.precision = MNN::BackendConfig::Precision_High;
        backend.power = MNN::BackendConfig::Power_Normal;
        MNN::ScheduleConfig config;
        config.type = MNN_FORWARD_CPU;
        config.numThread = 1;
        config.backendConfig = &backend;
        self->session = self->interpreter->createSession(config);
        if (!self->session) throw std::runtime_error("MNN could not create a CPU session");
        return self->interpreter->getSessionOutput(self->session, output_name);
    }
    Model(const void* bytes, size_t length, int id)
        : output(initialize(this, bytes, length, id == 197 ? "output" : "avgpool")) {
        const std::vector<const char*> names = id == 197
            ? std::vector<const char*>{"ctt_h_img", "ctt_l_img", "sty_l_img"}
            : std::vector<const char*>{"input"};
        const std::vector<std::vector<int>> shapes = id == 197
            ? std::vector<std::vector<int>>{{1,3,289,17},{1,3,256,256},{1,1,1,256}}
            : std::vector<std::vector<int>>{{1,3,224,224}};
        for (size_t i = 0; i < names.size(); ++i) {
            inputs.emplace_back(interpreter->getSessionInput(session, names[i]));
            if (inputs.back().device->shape() != shapes[i])
                throw std::runtime_error("MNN input shape differs from the verified model contract");
        }
        const std::vector<int> expected = id == 197
            ? std::vector<int>{1,3,289,17} : std::vector<int>{1,576};
        if (output.host->shape() != expected)
            throw std::runtime_error("MNN output shape differs from the verified model contract");
        interpreter->releaseModel();
    }
    void input(size_t index, const float* data, size_t length) {
        auto& tensor = inputs.at(index);
        if (!data || length != static_cast<size_t>(tensor.host->elementSize()))
            throw std::runtime_error("MNN input buffer length mismatch");
        std::copy_n(data, length, tensor.host->host<float>());
        if (!tensor.device->copyFromHostTensor(tensor.host.get()))
            throw std::runtime_error("MNN input transfer failed");
    }
    void run(float* data, size_t length) {
        if (!data || length != static_cast<size_t>(output.host->elementSize()))
            throw std::runtime_error("MNN output buffer length mismatch");
        if (interpreter->runSession(session) != MNN::NO_ERROR)
            throw std::runtime_error("MNN CPU inference failed");
        if (!output.device->copyToHostTensor(output.host.get()))
            throw std::runtime_error("MNN output transfer failed");
        const float* source = output.host->host<float>();
        for (size_t i = 0; i < length; ++i) {
            if (!std::isfinite(source[i])) throw std::runtime_error("MNN produced a non-finite value");
        }
        std::copy_n(source, length, data);
    }
};
void error(char* destination, size_t capacity, const char* message) noexcept {
    if (!destination || !capacity) return;
    const size_t count = std::min(capacity-1, std::strlen(message));
    std::memcpy(destination, message, count);
    destination[count] = '\0';
}
}

extern "C" {
const char* insta360_mnn_version() noexcept {
    return MNN::getVersion();
}

void* insta360_mnn_create(const unsigned char* bytes, size_t length, int id,
                         char* message, size_t capacity) noexcept {
    try {
        if (std::strcmp(MNN::getVersion(), "3.6.1") != 0)
            throw std::runtime_error("Linked MNN runtime must be version 3.6.1");
        if (!bytes || !length || (id != 197 && id != 198))
            throw std::runtime_error("Invalid underwater MNN model arguments");
        return new Model(bytes, length, id);
    } catch (const std::exception& e) { error(message, capacity, e.what()); }
      catch (...) { error(message, capacity, "Unknown MNN initialization failure"); }
    return nullptr;
}
int insta360_mnn_run(void* opaque, const float* first, size_t first_length,
                    const float* second, size_t second_length,
                    const float* third, size_t third_length,
                    float* output, size_t output_length,
                    char* message, size_t capacity) noexcept {
    try {
        if (!opaque) throw std::runtime_error("Missing MNN session");
        auto& model = *static_cast<Model*>(opaque);
        model.input(0, first, first_length);
        if (model.inputs.size() == 3) {
            model.input(1, second, second_length);
            model.input(2, third, third_length);
        }
        model.run(output, output_length);
        return 0;
    } catch (const std::exception& e) { error(message, capacity, e.what()); }
      catch (...) { error(message, capacity, "Unknown MNN inference failure"); }
    return -1;
}
void insta360_mnn_destroy(void* opaque) noexcept { delete static_cast<Model*>(opaque); }
}
