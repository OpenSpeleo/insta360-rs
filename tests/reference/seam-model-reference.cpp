// Independent numerical oracle: direct MNN API, without the Rust adapter/shim.
// Input file: four little-endian NCHW float tensors, repeated for each case.
// Output file: flow_f then flow_b as little-endian NCHW float tensors per case.
#include <MNN/Interpreter.hpp>
#include <MNN/Tensor.hpp>
#include <array>
#include <cmath>
#include <fstream>
#include <iostream>
#include <memory>
#include <stdexcept>
#include <string>

int main(int argc, char** argv) {
    try {
        if (argc != 5) throw std::runtime_error("model inputs outputs case-count required");
        if (std::string(MNN::getVersion()) != "3.6.1")
            throw std::runtime_error("MNN 3.6.1 required");
        std::unique_ptr<MNN::Interpreter> net(MNN::Interpreter::createFromFile(argv[1]));
        if (!net) throw std::runtime_error("model rejected");
        MNN::BackendConfig backend;
        backend.precision = MNN::BackendConfig::Precision_High;
        backend.power = MNN::BackendConfig::Power_Normal;
        MNN::ScheduleConfig schedule;
        schedule.type = MNN_FORWARD_CPU;
        schedule.numThread = 1;
        schedule.backendConfig = &backend;
        auto* session = net->createSession(schedule);
        if (!session) throw std::runtime_error("session failed");
        std::ifstream inputs(argv[2], std::ios::binary);
        std::ofstream outputs(argv[3], std::ios::binary);
        inputs.exceptions(std::ios::badbit | std::ios::failbit);
        outputs.exceptions(std::ios::badbit | std::ios::failbit);
        const std::array<const char*, 4> names{"input_0", "input_1", "mask_0", "mask_1"};
        for (int example = 0; example < std::stoi(argv[4]); ++example) {
            for (auto name : names) {
                auto* tensor = net->getSessionInput(session, name);
                if (!tensor) throw std::runtime_error("missing input");
                MNN::Tensor host(tensor, MNN::Tensor::CAFFE);
                inputs.read(reinterpret_cast<char*>(host.host<float>()), host.size());
                if (!tensor->copyFromHostTensor(&host)) throw std::runtime_error("input transfer failed");
            }
            if (net->runSession(session) != MNN::NO_ERROR)
                throw std::runtime_error("inference failed");
            for (auto name : {"flow_f", "flow_b"}) {
                auto* tensor = net->getSessionOutput(session, name);
                if (!tensor) throw std::runtime_error("missing output");
                MNN::Tensor host(tensor, MNN::Tensor::CAFFE);
                if (host.shape() != std::vector<int>{1, 2, 136, 16})
                    throw std::runtime_error("unexpected output shape");
                if (!tensor->copyToHostTensor(&host)) throw std::runtime_error("output transfer failed");
                for (int i = 0; i < host.elementSize(); ++i)
                    if (!std::isfinite(host.host<float>()[i])) throw std::runtime_error("nonfinite result");
                outputs.write(reinterpret_cast<const char*>(host.host<float>()), host.size());
            }
        }
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
