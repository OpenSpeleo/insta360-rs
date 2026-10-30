// Private C ABI around the independently built MNN 3.6.2 interpreter.
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
    std::vector<TensorBuffer> outputs;
    int backend_type = MNN_FORWARD_CPU;
    bool ready() const {
        int status = -1;
        return session && interpreter->getSessionInfo(session, MNN::Interpreter::RESIZE_STATUS, &status)
            && status == 0;
    }
    void initialize(const void* bytes, size_t length, int cpu_threads, int preference) {
        interpreter.reset(MNN::Interpreter::createFromBuffer(bytes, length));
        if (!interpreter) throw std::runtime_error("MNN rejected model data");
        MNN::BackendConfig backend;
        backend.precision = MNN::BackendConfig::Precision_High;
        backend.power = MNN::BackendConfig::Power_Normal;
        MNN::ScheduleConfig cpu;
        cpu.type = MNN_FORWARD_CPU;
        cpu.numThread = cpu_threads;
        cpu.backendConfig = &backend;
        MNN::ScheduleConfig gpu = cpu;
#if defined(__APPLE__)
        gpu.type = MNN_FORWARD_METAL;
#elif defined(__linux__) || defined(_WIN32)
        gpu.type = MNN_FORWARD_VULKAN;
#else
        gpu.type = MNN_FORWARD_CPU;
#endif
        // The GPU mode shares storage with numThread. Keep CPU budgets separate.
        gpu.mode = MNN_GPU_TUNING_WIDE;
        gpu.backupType = MNN_FORWARD_CPU;
        auto runtime = MNN::Interpreter::createRuntime(preference == 1
            ? std::vector<MNN::ScheduleConfig>{cpu}
            : std::vector<MNN::ScheduleConfig>{cpu, gpu});
        const bool available = gpu.type != MNN_FORWARD_CPU && runtime.first.count(gpu.type) != 0;
        if (preference == 2 && !available)
            throw std::runtime_error("MNN requested GPU backend is unavailable");
        const bool use_gpu = preference != 1 && available;
        session = interpreter->createSession(use_gpu ? gpu : cpu, runtime);
        if (!ready() && use_gpu && preference == 0) {
            if (session) interpreter->releaseSession(session);
            session = interpreter->createSession(cpu, runtime);
        }
        if (!ready()) throw std::runtime_error("MNN could not initialize the requested session");
        int backends[2] = {-1, -1};
        if (!interpreter->getSessionInfo(session, MNN::Interpreter::BACKENDS, backends))
            throw std::runtime_error("MNN did not report the session backend");
        // MNN exposes optimized x86/ARM CPU kernels as CPU_EXTENSION.
        backend_type = backends[0] == MNN_FORWARD_CPU_EXTENSION ? MNN_FORWARD_CPU : backends[0];
        if (preference == 2 && backend_type != gpu.type)
            throw std::runtime_error("MNN did not select the requested GPU backend");
    }
    Model(const void* bytes, size_t length, int id, int cpu_threads, int preference) {
        initialize(bytes, length, cpu_threads, preference);
        const std::vector<const char*> names = id == 197
            ? std::vector<const char*>{"ctt_h_img", "ctt_l_img", "sty_l_img"}
            : id == 198 ? std::vector<const char*>{"input"}
            : std::vector<const char*>{"input_0", "input_1", "mask_0", "mask_1"};
        const std::vector<std::vector<int>> shapes = id == 197
            ? std::vector<std::vector<int>>{{1,3,289,17},{1,3,256,256},{1,1,1,256}}
            : id == 198 ? std::vector<std::vector<int>>{{1,3,224,224}}
            : std::vector<std::vector<int>>{{1,3,544,64},{1,3,544,64},{1,1,544,64},{1,1,544,64}};
        for (size_t i = 0; i < names.size(); ++i) {
            inputs.emplace_back(interpreter->getSessionInput(session, names[i]));
            if (inputs.back().host->shape() != shapes[i])
                throw std::runtime_error("MNN input shape differs from the verified model contract");
        }
        const std::vector<const char*> output_names = id == 197
            ? std::vector<const char*>{"output"}
            : id == 198 ? std::vector<const char*>{"avgpool"}
            : std::vector<const char*>{"flow_f", "flow_b"};
        const std::vector<int> expected = id == 197
            ? std::vector<int>{1,3,289,17} : id == 198
            ? std::vector<int>{1,576} : std::vector<int>{1,2,136,16};
        for (auto name : output_names) {
            outputs.emplace_back(interpreter->getSessionOutput(session, name));
            if (outputs.back().host->shape() != expected)
                throw std::runtime_error("MNN output shape differs from the verified model contract");
        }
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
    void run(float* const* data, const size_t* lengths, size_t count) {
        if (!data || !lengths || count != outputs.size())
            throw std::runtime_error("MNN output count mismatch");
        for (size_t n = 0; n < count; ++n) {
            if (!data[n] || lengths[n] != static_cast<size_t>(outputs[n].host->elementSize()))
                throw std::runtime_error("MNN output buffer length mismatch");
        }
        if (interpreter->runSession(session) != MNN::NO_ERROR)
            throw std::runtime_error("MNN inference failed");
        // Validate every result before publishing any caller-owned tensor.
        for (auto& output : outputs) {
            if (!output.device->copyToHostTensor(output.host.get()))
                throw std::runtime_error("MNN output transfer failed");
            const float* source = output.host->host<float>();
            for (int i = 0; i < output.host->elementSize(); ++i) {
                if (!std::isfinite(source[i])) throw std::runtime_error("MNN produced a non-finite value");
            }
        }
        for (size_t n = 0; n < count; ++n)
            std::copy_n(outputs[n].host->host<float>(), lengths[n], data[n]);
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

void* insta360_mnn_create(const unsigned char* bytes, size_t length, int id, int cpu_threads, int preference,
                         char* message, size_t capacity) noexcept {
    try {
        if (std::strcmp(MNN::getVersion(), "3.6.2") != 0)
            throw std::runtime_error("Linked MNN runtime must be version 3.6.2");
        if (!bytes || !length || (id != 197 && id != 198 && id != 213))
            throw std::runtime_error("Invalid MNN model arguments");
        if (cpu_threads < 1 || cpu_threads > 4)
            throw std::runtime_error("MNN CPU thread count must be in 1..=4");
        if (preference < 0 || preference > 2)
            throw std::runtime_error("Invalid MNN backend preference");
        return new Model(bytes, length, id, cpu_threads, preference);
    } catch (const std::exception& e) { error(message, capacity, e.what()); }
      catch (...) { error(message, capacity, "Unknown MNN initialization failure"); }
    return nullptr;
}
int insta360_mnn_run(void* opaque, const float* const* inputs,
                    const size_t* input_lengths, size_t input_count,
                    float* const* outputs, const size_t* output_lengths, size_t output_count,
                    char* message, size_t capacity) noexcept {
    try {
        if (!opaque || !inputs || !input_lengths) throw std::runtime_error("Missing MNN session or input");
        auto& model = *static_cast<Model*>(opaque);
        if (input_count != model.inputs.size()) throw std::runtime_error("MNN input count mismatch");
        for (size_t n = 0; n < input_count; ++n) model.input(n, inputs[n], input_lengths[n]);
        model.run(outputs, output_lengths, output_count);
        return 0;
    } catch (const std::exception& e) { error(message, capacity, e.what()); }
      catch (...) { error(message, capacity, "Unknown MNN inference failure"); }
    return -1;
}
int insta360_mnn_backend(void* opaque) noexcept {
    return opaque ? static_cast<Model*>(opaque)->backend_type : -1;
}
// Used by numerical tests and isolated phase diagnostics. The public MNN query
// reads runtime-adjusted pipeline info, including process-global pool clamping.
int insta360_mnn_actual_threads(void* opaque) noexcept {
    if (!opaque) return 0;
    try {
        auto& model = *static_cast<Model*>(opaque);
        if (model.backend_type != MNN_FORWARD_CPU) return 0;
        int threads = 0;
        if (!model.interpreter->getSessionInfo(model.session, MNN::Interpreter::THREAD_NUMBER, &threads))
            return 0;
        return threads;
    } catch (...) { return 0; }
}
void insta360_mnn_destroy(void* opaque) noexcept { delete static_cast<Model*>(opaque); }
}
